//! Quickstart apply path.
//!
//! Single entry point both surfaces (web gateway, zerocode RPC, CLI)
//! call to land a [`BuilderSubmission`] into the live [`Config`]. The
//! runtime never enumerates channel types, provider types, or storage
//! backends itself — every write goes through `Config::set_prop_persistent`,
//! which dispatches through the schema-derived `Configurable` tree.
//! Adding a new channel / provider / storage backend to the schema
//! lights up in the Quickstart for free.

use serde::{Deserialize, Serialize};

use zeroclaw_config::helpers::kebab_to_snake;
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice, risk_preset, runtime_preset,
};
use zeroclaw_config::schema::Config;

/// Which surface invoked the Quickstart. Stamped on every event in
/// the apply path so SSE/dashboard consumers can filter by origin
/// without parsing message strings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Web,
    Tui,
    Cli,
    Test,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Web => "web",
            Surface::Tui => "tui",
            Surface::Cli => "cli",
            Surface::Test => "test",
        }
    }
}

/// Per-run attribution carried through the apply path so every emitted
/// event lands with the same correlation id. Constructed by `apply`
/// and `validate_only`; threaded down into `apply_into` and the
/// per-selector helpers via `&RunCtx`.
struct RunCtx {
    run_id: String,
    surface: Surface,
}

impl RunCtx {
    fn new(surface: Surface) -> Self {
        // Fall back to nanosecond timestamp if a system without a clock
        // is somehow in play. Either way the id is unique per process.
        let run_id = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| format!("{:x}{:x}", d.as_secs(), d.subsec_nanos()))
            .unwrap_or_else(|_| format!("{:x}", std::process::id()));
        Self { run_id, surface }
    }

    fn base_attrs(&self) -> serde_json::Value {
        serde_json::json!({
            "quickstart.run_id": self.run_id,
            "quickstart.surface": self.surface.as_str(),
        })
    }
}

/// Layer per-event attrs on top of the run-scoped base. Both must be
/// JSON objects; non-object inputs return `base` unchanged.
fn merge_attrs(base: serde_json::Value, extra: serde_json::Value) -> serde_json::Value {
    let (mut base_map, extra_map) = match (base, extra) {
        (serde_json::Value::Object(b), serde_json::Value::Object(e)) => (b, e),
        (b, _) => return b,
    };
    for (k, v) in extra_map {
        base_map.insert(k, v);
    }
    serde_json::Value::Object(base_map)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppliedAgent {
    pub alias: String,
    pub model_provider: String,
    pub risk_profile: String,
    pub runtime_profile: String,
    pub channels: Vec<String>,
    pub memory_backend: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum QuickstartStep {
    ModelProvider,
    RiskProfile,
    RuntimeProfile,
    Memory,
    Channels,
    PeerGroups,
    Agent,
}

impl QuickstartStep {
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::ModelProvider => "Model provider",
            Self::RiskProfile => "Risk profile",
            Self::RuntimeProfile => "Runtime profile",
            Self::Memory => "Memory",
            Self::Channels => "Channels",
            Self::PeerGroups => "Peer groups",
            Self::Agent => "Agent",
        }
    }

    #[must_use]
    pub fn label_key(self) -> &'static str {
        match self {
            Self::ModelProvider => "cli-quickstart-step-model-provider",
            Self::RiskProfile => "cli-quickstart-step-risk-profile",
            Self::RuntimeProfile => "cli-quickstart-step-runtime-profile",
            Self::Memory => "cli-quickstart-step-memory",
            Self::Channels => "cli-quickstart-step-channels",
            Self::PeerGroups => "cli-quickstart-step-peer-groups",
            Self::Agent => "cli-quickstart-step-agent",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuickstartError {
    pub step: QuickstartStep,
    pub field: String,
    pub message: String,
}

impl QuickstartError {
    fn new(step: QuickstartStep, field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            step,
            field: field.into(),
            message: message.into(),
        }
    }

    fn for_surface(
        ctx: Option<&RunCtx>,
        step: QuickstartStep,
        field: impl Into<String>,
        fallback: impl Into<String>,
        key: &str,
        args: &[(&str, &str)],
    ) -> Self {
        let fallback = fallback.into();
        if !matches!(ctx.map(|ctx| ctx.surface), Some(Surface::Cli)) {
            return Self::new(step, field, fallback);
        }

        Self::new(
            step,
            field,
            crate::i18n::get_required_cli_string_with_args(key, args),
        )
    }
}

impl std::fmt::Display for QuickstartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.field.is_empty() {
            write!(f, "{:?}: {}", self.step, self.message)
        } else {
            write!(f, "{:?}.{}: {}", self.step, self.field, self.message)
        }
    }
}

pub fn validate_only(
    submission: &BuilderSubmission,
    config: &Config,
) -> Result<(), Vec<QuickstartError>> {
    validate_only_with_surface(submission, config, Surface::Web)
}

pub fn validate_only_with_surface(
    submission: &BuilderSubmission,
    config: &Config,
    surface: Surface,
) -> Result<(), Vec<QuickstartError>> {
    let ctx = RunCtx::new(surface);
    let mut staged = config.clone();
    let mut errors = Vec::new();
    // validate-only never commits; staged tempfiles drop at scope exit.
    let mut staged_files = Vec::new();
    apply_into(
        &mut staged,
        submission,
        &mut staged_files,
        &mut errors,
        Some(&ctx),
    );
    let ok = errors.is_empty();
    let attrs = merge_attrs(
        ctx.base_attrs(),
        serde_json::json!({"error_count": errors.len()}),
    );
    let outcome = if ok {
        ::zeroclaw_log::EventOutcome::Success
    } else {
        ::zeroclaw_log::EventOutcome::Failure
    };
    if ok {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "quickstart: validate_only"
        );
    } else {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Validate)
                .with_outcome(outcome)
                .with_attrs(attrs),
            "quickstart: validate_only"
        );
    }
    if ok { Ok(()) } else { Err(errors) }
}

pub async fn apply(
    submission: BuilderSubmission,
    config: &mut Config,
) -> Result<AppliedAgent, Vec<QuickstartError>> {
    apply_with_surface(submission, config, Surface::Web).await
}

pub async fn apply_with_surface(
    submission: BuilderSubmission,
    config: &mut Config,
    surface: Surface,
) -> Result<AppliedAgent, Vec<QuickstartError>> {
    let ctx = RunCtx::new(surface);
    let started = std::time::Instant::now();

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Start)
            .with_attrs(ctx.base_attrs()),
        "quickstart: apply"
    );

    let mut errors = Vec::new();
    let mut staged_files = Vec::new();
    let applied = apply_into(
        config,
        &submission,
        &mut staged_files,
        &mut errors,
        Some(&ctx),
    );
    if !errors.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "error_count": errors.len(),
                        "elapsed_ms": started.elapsed().as_millis() as u64,
                    }),
                )),
            "quickstart: apply rejected"
        );
        return Err(errors);
    }
    let applied = match applied {
        Some(applied) => applied,
        None => {
            return Err(vec![QuickstartError::for_surface(
                Some(&ctx),
                QuickstartStep::Agent,
                "apply",
                "internal error: apply_into returned no result despite no validation errors",
                "cli-quickstart-error-internal-no-result",
                &[],
            )]);
        }
    };

    config
        .set_prop_persistent("onboard_state.quickstart_completed", "true")
        .map_err(|err| {
            vec![QuickstartError::for_surface(
                Some(&ctx),
                QuickstartStep::Agent,
                "",
                format!("failed to flip quickstart-completed: {err}"),
                "cli-quickstart-error-completion-flag",
                &[("err", &err.to_string())],
            )]
        })?;
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({"flag": "quickstart_completed"}),
            )
        ),
        "quickstart: completion flag flipped"
    );

    let dirty_count = config.dirty_paths.len();
    let write_started = std::time::Instant::now();
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({"dirty_path_count": dirty_count}),
            )
        ),
        "quickstart: persist start"
    );
    let write_result = config.save_dirty().await;
    let write_ms = write_started.elapsed().as_millis() as u64;
    match &write_result {
        Ok(_) => ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_outcome(::zeroclaw_log::EventOutcome::Success)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "dirty_path_count": dirty_count,
                        "elapsed_ms": write_ms,
                    }),
                )),
            "quickstart: persist complete"
        ),
        Err(err) => ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Write)
                .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                .with_attrs(merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "dirty_path_count": dirty_count,
                        "elapsed_ms": write_ms,
                        "error": err.to_string(),
                    }),
                )),
            "quickstart: persist failed"
        ),
    }
    write_result.map_err(|err| {
        vec![QuickstartError::for_surface(
            Some(&ctx),
            QuickstartStep::Agent,
            "",
            format!("failed to persist config: {err}"),
            "cli-quickstart-error-persist-config",
            &[("err", &err.to_string())],
        )]
    })?;

    // Config landed atomically — now move the staged personality files
    // into place. Any failure here is reported but does not unwind the
    // already-persisted config; the agent is valid without them.
    let mut commit_errors = Vec::new();
    commit_personality_files(staged_files, &mut commit_errors, Some(&ctx));
    if !commit_errors.is_empty() {
        return Err(commit_errors);
    }

    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Complete)
            .with_outcome(::zeroclaw_log::EventOutcome::Success)
            .with_attrs(merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({
                    "agent": applied.alias,
                    "channels": applied.channels.len(),
                    "elapsed_ms": started.elapsed().as_millis() as u64,
                }),
            )),
        "quickstart: apply complete"
    );
    Ok(applied)
}

/// Record a `dismissed` event for a run that exited without a
/// Create. Surfaces call this when the user closes the Quickstart
/// page / leaves the modal stack before submitting. `last_step` is
/// optional and names whichever selector the user got furthest with;
/// pass `None` for "didn't progress past the first selector."
pub fn record_dismissed(run_id: &str, surface: Surface, last_step: Option<QuickstartStep>) {
    let last_step_str = last_step
        .map(|s| match s {
            QuickstartStep::ModelProvider => "model_provider",
            QuickstartStep::RiskProfile => "risk_profile",
            QuickstartStep::RuntimeProfile => "runtime_profile",
            QuickstartStep::Memory => "memory",
            QuickstartStep::Channels => "channels",
            QuickstartStep::PeerGroups => "peer_groups",
            QuickstartStep::Agent => "agent",
        })
        .unwrap_or("none");
    ::zeroclaw_log::record!(
        INFO,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "quickstart.run_id": run_id,
                "quickstart.surface": surface.as_str(),
                "last_step": last_step_str,
                "dismissed": true,
            })),
        "quickstart: dismissed"
    );
}

/// `onboard_state.quickstart_completed` is false **and** no
/// `agents.*` entries exist. Returning users with existing agents
/// never see the auto-trigger even if the flag was never flipped.
pub fn should_auto_launch(config: &Config) -> bool {
    !config.onboard_state.quickstart_completed && config.agents.is_empty()
}

/// Snapshot of the bits of `Config` the Quickstart UI needs to render
/// each step's "Use existing" section without pulling the entire config.
///
/// Shared by every surface — the gateway's `GET /api/quickstart/state`
/// and the RPC `quickstart/state` method both build the response from
/// this one function, so the two transports cannot drift.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartState {
    pub quickstart_completed: bool,
    pub agents: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    /// `<provider_type>.<alias>` refs for every configured model provider.
    pub model_providers: Vec<String>,
    /// `<channel_type>.<alias>` refs.
    pub channels: Vec<String>,
    /// Subset of `channels` that is not yet bound to any agent's
    /// `agents.<alias>.channels` field. Surfaces use this for "Use
    /// existing" pickers so they cannot let the user accidentally
    /// reassign a channel that's still owned by another agent
    /// (the schema invariant is one channel → one agent).
    #[serde(default)]
    pub unassigned_channels: Vec<String>,
    /// `<storage_type>.<alias>` refs.
    pub storage: Vec<String>,
    /// Available model-provider types the Quickstart "Create new"
    /// picker can offer. Derived at request time from the canonical
    /// registry in `zeroclaw_providers::list_model_providers()` — the
    /// same source the CLI catalog and gateway sections route use.
    /// Surfaces render this list as-is; they do not maintain their own.
    pub model_provider_types: Vec<QuickstartTypeOption>,
    /// Available channel kinds the Quickstart "Create new" picker can
    /// offer. Derived at request time from
    /// [`zeroclaw_config::schema::ChannelsConfig::channels`] — the
    /// schema-side single source of truth for "what channel kinds the
    /// config schema knows about." Compile-time gating of channel
    /// implementations (via `zeroclaw-channels` features) is enforced
    /// later, at apply time; the picker shows every kind the schema
    /// can represent so users get a consistent option list across
    /// builds.
    pub channel_types: Vec<QuickstartTypeOption>,
    /// Risk presets from `zeroclaw_config::presets::RISK_PRESETS`.
    pub risk_presets: &'static [zeroclaw_config::presets::RiskPreset],
    /// Runtime presets from `zeroclaw_config::presets::RUNTIME_PRESETS`.
    pub runtime_presets: &'static [zeroclaw_config::presets::RuntimePreset],
    /// Memory backend snake-case kinds from `MemoryBackendKind`.
    pub memory_kinds: Vec<String>,
    /// Canonical personality filenames the Quickstart will accept.
    /// Surfaces iterate this; never hardcode the filename list.
    pub personality_files: &'static [&'static str],
}

/// One row in the Quickstart "Create new …" picker, sourced from a
/// schema- or registry-level inventory so neither the TUI nor the web
/// surface needs its own list. `kind` is the canonical kebab-case
/// identifier written into config; `display_name` is the picker label.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartTypeOption {
    /// Canonical identifier (e.g. `"anthropic"`, `"telegram"`).
    pub kind: String,
    /// Human-readable picker label (e.g. `"Anthropic"`, `"Telegram"`).
    pub display_name: String,
    /// `true` when the entry runs locally and needs no remote
    /// credential. Channels always report `false`; providers reflect
    /// their `local` flag from `ModelProviderInfo`.
    pub local: bool,
}

/// Build a [`QuickstartState`] snapshot from the live config.
///
/// The two `*_types` lists are populated from the canonical sources
/// (`zeroclaw_providers::list_model_providers()` for providers,
/// `cfg.channels.channels()` for channel kinds). Adding a new entry in
/// either source automatically lights up here — no Quickstart code
/// change required. This is the DRY contract the plan calls out under
/// "Reads the per-provider field map at render time so adding a
/// provider in the schema doesn't require Quickstart code changes."
pub fn snapshot_state(cfg: &Config) -> QuickstartState {
    let model_provider_types = zeroclaw_providers::list_model_providers()
        .into_iter()
        .map(|info| QuickstartTypeOption {
            kind: info.name.to_string(),
            display_name: info.display_name.to_string(),
            local: info.local,
        })
        .collect();
    // Channel kinds come from the schema-side inventory. The
    // serde-shaped `ChannelsConfig` is an object whose top-level
    // keys are the kebab-case channel kinds (`telegram`, `discord`,
    // `wecom-ws`, …). We walk that shape — same technique
    // `collect_aliased_refs` uses below — so adding a new channel
    // family in the schema lights up here for free. Display names
    // are looked up from `ChannelsConfig::channels()` by index so we
    // don't drift between the two views; if `channels()` returns
    // fewer rows than the schema has top-level keys, the missing
    // ones fall back to their kebab-case kind for display.
    let channel_types = build_channel_type_options(&cfg.channels);
    QuickstartState {
        quickstart_completed: cfg.onboard_state.quickstart_completed,
        agents: cfg.agents.keys().cloned().collect(),
        risk_profiles: cfg.risk_profiles.keys().cloned().collect(),
        runtime_profiles: cfg.runtime_profiles.keys().cloned().collect(),
        model_providers: cfg
            .providers
            .models
            .iter_entries()
            .map(|(family, alias, _)| format!("{family}.{alias}"))
            .collect(),
        channels: collect_aliased_refs(&cfg.channels),
        // Channel refs that are not yet bound to any agent. The
        // schema enforces one-channel-one-agent; surfacing already-
        // owned channels in a "Use existing" picker would silently
        // break that invariant. Surfaces should always present this
        // list (not the raw `channels` list) when offering reuse.
        unassigned_channels: collect_aliased_refs(&cfg.channels)
            .into_iter()
            .filter(|ch| cfg.agent_for_channel(ch).is_none())
            .collect(),
        storage: collect_aliased_refs(&cfg.storage),
        model_provider_types,
        channel_types,
        risk_presets: zeroclaw_config::presets::RISK_PRESETS,
        runtime_presets: zeroclaw_config::presets::RUNTIME_PRESETS,
        memory_kinds: memory_kind_keys(),
        personality_files: crate::agent::personality::EDITABLE_PERSONALITY_FILES,
    }
}

/// Snake-case wire keys for every `MemoryBackendKind` variant. Exhaustive
/// match probe catches missing variants at compile time; serde produces
/// the wire key so there's no parallel mapping.
fn memory_kind_keys() -> Vec<String> {
    use zeroclaw_config::multi_agent::MemoryBackendKind as M;
    [
        M::Sqlite,
        M::Markdown,
        M::Postgres,
        M::Qdrant,
        M::Lucid,
        M::None,
    ]
    .into_iter()
    .map(|k| {
        // Exhaustiveness guard: adding a new variant forces this match to fail
        // to compile until the contributor decides whether the new backend
        // belongs in the quickstart picker.
        match k {
            M::Sqlite | M::Markdown | M::Postgres | M::Qdrant | M::Lucid | M::None => (),
        }
        serde_json::to_value(k)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default()
    })
    .collect()
}

/// Build the Quickstart channel-type picker rows directly from the
/// schema's curated `ChannelsConfig::channels()` list. Each entry
/// already carries its canonical kebab-case `kind` and human label,
/// so the surface never re-derives them from serde introspection
/// (which loses unconfigured channels because of
/// `#[serde(skip_serializing_if = "HashMap::is_empty")]`).
fn build_channel_type_options(
    channels_cfg: &zeroclaw_config::schema::ChannelsConfig,
) -> Vec<QuickstartTypeOption> {
    channels_cfg
        .channels()
        .into_iter()
        .map(|info| QuickstartTypeOption {
            kind: info.kind.to_string(),
            display_name: info.name.to_string(),
            local: false,
        })
        .collect()
}

/// Walk the serialised form of `value` and yield `<type>.<alias>` refs
/// for every `HashMap<String, _>`-shaped subsection. Schema-driven —
/// adding a new channel or storage slot in the schema lights up here
/// for free, no code change required.
fn collect_aliased_refs<T: serde::Serialize>(value: &T) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(serde_json::Value::Object(map)) = serde_json::to_value(value) else {
        return out;
    };
    for (family, subvalue) in map {
        if let serde_json::Value::Object(entries) = subvalue {
            for alias in entries.keys() {
                out.push(format!("{family}.{alias}"));
            }
        }
    }
    out.sort();
    out
}

/// Selector kinds that the Quickstart "field shape" descriptor
/// covers. The TUI / web ask the runtime for the shape, then render
/// inputs dumbly off the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldSection {
    ModelProvider,
    Channel,
    PeerGroup,
}

/// One renderable input the TUI / web modal must draw.
///
/// Shape is derived from `prop_fields()` filtered by the relevant
/// schema prefix, then trimmed to the "greatest hits" required for
/// Quickstart per [`field_shape`]. Surfaces never invent fields —
/// adding a provider or channel kind to the schema lights up here
/// automatically.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FieldDescriptor {
    /// Schema-side field key (kebab-case terminal segment). The
    /// caller submits this back through [`BuilderSubmission`].
    pub key: String,
    /// Human label shown next to the input.
    pub label: String,
    /// One-line help blurb. Empty when the schema field has no doc.
    pub help: String,
    /// Wire-tag for the input control to render. Mirrors
    /// `PropKind::wire_name`.
    pub kind: zeroclaw_config::traits::PropKind,
    /// `true` for `#[secret]` fields — the modal masks input.
    pub is_secret: bool,
    /// Closed-set choices for `Enum` kind. `None` for everything else.
    pub enum_variants: Option<Vec<String>>,
    /// `true` when Quickstart treats this field as required. Currently
    /// every field returned by [`field_shape`] is required, but the
    /// flag is exposed so future additions can include optional rows.
    pub required: bool,
    /// Pre-filled default the modal should show as ghost text /
    /// initial input value. `None` when the schema has no meaningful
    /// default for this field (e.g. API keys, bot tokens).
    pub default: Option<String>,
}

/// Return the renderable field shape for a single section + type
/// combination. Walks `prop_fields()` against a synthetic config with
/// one default-instantiated entry under the requested type, then
/// filters to the per-section "essential" allowlist.
pub fn field_shape(section: FieldSection, type_key: &str) -> Vec<FieldDescriptor> {
    // Probe alias for the synthetic field-shape lookup. Must satisfy
    // `validate_alias_key` (lowercase alphanumeric + underscore, can't
    // start/end with `_`, no `__`) — otherwise `create_map_key` returns
    // an alias-validation Err that the recurse arms in the Configurable
    // derive mask as "no map-keyed/list section", and field_shape
    // silently returns an empty Vec.
    const SYNTHETIC_ALIAS: &str = "qs0probe";
    let (section_path, essentials) = match section {
        FieldSection::ModelProvider => (
            format!("providers.models.{type_key}"),
            MODEL_PROVIDER_ESSENTIALS,
        ),
        FieldSection::Channel => (format!("channels.{type_key}"), CHANNEL_ESSENTIALS),
        FieldSection::PeerGroup => ("peer_groups".to_string(), PEER_GROUP_ESSENTIALS),
    };

    // A throwaway Config we can mutate freely. Inject one default
    // entry under the requested type so `prop_fields()` enumerates
    // its leaves.
    let mut probe = Config::default();
    if probe
        .create_map_key(&section_path, SYNTHETIC_ALIAS)
        .is_err()
    {
        return Vec::new();
    }
    let leaf_prefix = format!("{section_path}.{SYNTHETIC_ALIAS}.");

    let mut out = Vec::new();
    for info in probe.prop_fields() {
        let Some(field_path) = info.name.strip_prefix(&leaf_prefix) else {
            continue;
        };
        if !essentials.contains(&field_path) {
            continue;
        }
        // `display_value` already masks secrets as `****`; we want
        // ghost-text defaults for plain fields only. `<unset>` is the
        // placeholder for an unset Option, not a real value — emitting
        // it as a default makes every surface (CLI, TUI, web) echo it
        // back into the submission, where the daemon then validates
        // `<unset>` against the field's true type (e.g. a bool, which
        // fails with "length 7"). Treat it like an empty default.
        let default = if info.is_secret {
            None
        } else {
            let raw = info.display_value.trim();
            if raw.is_empty() || raw == zeroclaw_config::traits::UNSET_DISPLAY {
                None
            } else {
                Some(raw.to_string())
            }
        };
        out.push(FieldDescriptor {
            key: field_path.to_string(),
            label: kebab_to_snake(field_path),
            help: info.description.trim().to_string(),
            kind: info.kind,
            is_secret: info.is_secret,
            enum_variants: info.enum_variants.map(|f| f()),
            // `uri` is an override-only field — operators set it only
            // when pointing at a self-hosted gateway. `requires_openai_auth`
            // and `wire_api` are OpenAI Codex subscription fields — optional
            // for all providers, meaningful only for OpenAI. `api_key` is
            // left non-required because local providers (Ollama) and Codex
            // subscription auth don't need one — the runtime surfaces a
            // clear error at request time if a remote provider is missing
            // its key. Everything else in the essentials list is required
            // to actually issue a request.
            required: !matches!(
                field_path,
                "uri" | "api_key" | "requires_openai_auth" | "wire_api"
            ),
            default,
        });
    }
    out.sort_by_key(|d| {
        essentials
            .iter()
            .position(|k| *k == d.key.as_str())
            .unwrap_or(usize::MAX)
    });
    out
}

/// Essentials per section kind. Kept in one place so adding a
/// provider type or channel kind lights up Quickstart for free,
/// while keeping the modal focused on what an agent cannot start
/// without.
const MODEL_PROVIDER_ESSENTIALS: &[&str] = &[
    "model",
    "api_key",
    "uri",
    "requires_openai_auth",
    "wire_api",
];
const CHANNEL_ESSENTIALS: &[&str] = &["bot_token", "token", "webhook_url", "allowed_users"];
const PEER_GROUP_ESSENTIALS: &[&str] = &["channel", "external_peers", "agents", "ignore"];

/// Runtime profile the Quickstart silently installs. The Runtime Profile
/// picker was removed from every surface; apply always writes this preset.
const FORCED_RUNTIME_PRESET: &str = "unbounded";

fn apply_into(
    config: &mut Config,
    submission: &BuilderSubmission,
    staged_files: &mut Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<AppliedAgent> {
    let provider_ref = apply_model_provider(config, &submission.model_provider, errors, ctx)?;
    emit_selector_pick(
        ctx,
        "model_provider",
        selector_mode(&submission.model_provider),
        &provider_ref,
    );

    let risk_alias = apply_named_preset(
        config,
        &submission.risk_profile,
        QuickstartStep::RiskProfile,
        risk_preset_keys,
        write_risk_preset,
        errors,
        ctx,
    )?;
    emit_selector_pick(
        ctx,
        "risk_profile",
        selector_mode(&submission.risk_profile),
        &risk_alias,
    );

    let runtime_alias = match write_runtime_preset(config, FORCED_RUNTIME_PRESET) {
        Ok(alias) => alias,
        Err(msg) => {
            errors.push(QuickstartError::new(
                QuickstartStep::RuntimeProfile,
                "",
                msg,
            ));
            return None;
        }
    };
    emit_selector_pick(
        ctx,
        "runtime_profile",
        selector_mode(&submission.runtime_profile),
        &runtime_alias,
    );

    let memory_backend = apply_memory(config, &submission.memory, errors, ctx)?;
    emit_selector_pick(
        ctx,
        "memory",
        selector_mode(&submission.memory),
        &memory_backend,
    );

    let channel_refs = apply_channels(config, &submission.channels, errors, ctx);
    if let Some(ctx) = ctx {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "selector": "channels",
                        "count": channel_refs.len(),
                    }),
                )
            ),
            "quickstart: selector channels"
        );
    }

    if !errors.is_empty() {
        return None;
    }
    let alias = apply_agent(
        config,
        &submission.agent,
        &provider_ref,
        &risk_alias,
        &runtime_alias,
        &channel_refs,
        errors,
        ctx,
    )?;
    emit_selector_pick(ctx, "agent", "create_new", &alias);

    let peer_group_refs =
        apply_peer_groups(config, &submission.peer_groups, &channel_refs, errors, ctx);
    if let Some(ctx) = ctx {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
                merge_attrs(
                    ctx.base_attrs(),
                    serde_json::json!({
                        "selector": "peer_groups",
                        "count": peer_group_refs.len(),
                    }),
                )
            ),
            "quickstart: selector peer_groups"
        );
    }

    apply_personality_files(
        config,
        &alias,
        &submission.agent.personality_files,
        staged_files,
        errors,
        ctx,
    );

    materialize_default_skills_bundle(config);

    if !errors.is_empty() {
        return None;
    }

    Some(AppliedAgent {
        alias,
        model_provider: provider_ref,
        risk_profile: risk_alias,
        runtime_profile: runtime_alias,
        channels: channel_refs,
        memory_backend,
    })
}

/// Surface representation of a selector's submission mode for
/// observability. We never inspect the wrapped value here — only
/// whether the user picked an existing alias or created fresh.
fn selector_mode<T>(choice: &SelectorChoice<T>) -> &'static str {
    match choice {
        SelectorChoice::Existing(_) => "use_existing",
        SelectorChoice::Fresh(_) => "create_new",
    }
}

fn emit_selector_pick(ctx: Option<&RunCtx>, selector: &str, mode: &str, value: &str) {
    let Some(ctx) = ctx else { return };
    ::zeroclaw_log::record!(
        DEBUG,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(
            merge_attrs(
                ctx.base_attrs(),
                serde_json::json!({
                    "selector": selector,
                    "mode": mode,
                    "value": value,
                }),
            )
        ),
        "quickstart: selector pick"
    );
}

// ── Model provider ─────────────────────────────────────────────────

fn apply_model_provider(
    config: &mut Config,
    choice: &SelectorChoice<ModelProviderChoice>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::ModelProvider,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                    return None;
                }
            };
            if !section_has_alias(config, "providers.models", family, alias) {
                let path = format!("providers.models.{family}.{alias}");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "",
                    format!("no `{path}` configured"),
                    "cli-quickstart-error-no-configured-path",
                    &[("path", &path)],
                ));
                return None;
            }
            Some(reference.clone())
        }
        SelectorChoice::Fresh(choice) => {
            if choice.provider_type.trim().is_empty()
                || choice.alias.trim().is_empty()
                || choice.model.trim().is_empty()
            {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "",
                    "provider type, alias, and model are required",
                    "cli-quickstart-error-provider-required",
                    &[],
                ));
                return None;
            }
            // Canonicalize the provider type against the registry. The picker
            // offers canonical `info.name` keys, but a hand-typed or
            // whitespace-padded value (e.g. "llamacpp ", "llama.cpp") would
            // otherwise reach `create_map_key` verbatim and fail with a cryptic
            // "no map-keyed/list section" because the family key doesn't match.
            let provider_type = choice.provider_type.trim();
            let provider_type = match zeroclaw_providers::list_model_providers()
                .into_iter()
                .find(|info| info.name.eq_ignore_ascii_case(provider_type))
            {
                Some(info) => info.name.to_string(),
                None => {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::ModelProvider,
                        "provider_type",
                        format!(
                            "unknown model provider type `{}` — pick one from the provider list",
                            choice.provider_type.trim()
                        ),
                        "cli-quickstart-error-unknown-provider-type",
                        &[("provider", choice.provider_type.trim())],
                    ));
                    return None;
                }
            };
            if section_has_alias(config, "providers.models", &provider_type, &choice.alias) {
                let alias_ref = format!("{}.{}", provider_type, choice.alias);
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    "alias",
                    format!("alias `{alias_ref}` already exists"),
                    "cli-quickstart-error-alias-exists",
                    &[("alias", &alias_ref)],
                ));
                return None;
            }
            let prefix = format!("providers.models.{}.{}", provider_type, choice.alias);
            if let Err(err) = config.create_map_key(
                &format!("providers.models.{}", provider_type),
                &choice.alias,
            ) {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "provider_type",
                    err.to_string(),
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent(&format!("{prefix}.model"), &choice.model)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "model",
                    err.to_string(),
                ));
                return None;
            }
            // Round-trip every field the surface echoed back. Keys are
            // whatever `field_shape()` emitted — the daemon authored
            // them, so it knows where they go.
            let mut entries: Vec<(&String, &String)> = choice.fields.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in entries {
                if value.is_empty() {
                    continue;
                }
                if let Err(err) = config.set_prop_persistent(&format!("{prefix}.{key}"), value) {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        zeroclaw_config::helpers::kebab_to_snake(key),
                        err.to_string(),
                    ));
                    return None;
                }
            }
            // Auto-populate context_window from provider's /models endpoint if supported.
            // Silently ignores failures (falls back to config default).
            // Only runs when there's a Tokio runtime (i.e., actual CLI, not tests).
            let provider_config = zeroclaw_config::schema::ModelProviderConfig {
                model: Some(choice.model.clone()),
                uri: config.get_prop(&format!("{prefix}.uri")).ok(),
                ..Default::default()
            };
            if tokio::runtime::Handle::try_current().is_ok()
                && let Some(ctx) = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(
                        zeroclaw_providers::fetch_context_window(&provider_type, &provider_config),
                    )
                })
            {
                let _ = config
                    .set_prop_persistent(&format!("{prefix}.context_window"), &ctx.to_string());
            }
            Some(format!("{}.{}", provider_type, choice.alias))
        }
    }
}

// ── Risk / Runtime presets ─────────────────────────────────────────

fn apply_named_preset<K, W>(
    config: &mut Config,
    choice: &SelectorChoice<String>,
    step: QuickstartStep,
    list_existing: K,
    write_preset: W,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String>
where
    K: Fn(&Config) -> Vec<String>,
    W: Fn(&mut Config, &str) -> Result<String, String>,
{
    match choice {
        SelectorChoice::Existing(alias) => {
            if list_existing(config).iter().any(|a| a == alias) {
                Some(alias.clone())
            } else {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    step,
                    "",
                    format!("no `{alias}` profile configured"),
                    "cli-quickstart-error-no-profile",
                    &[("alias", alias)],
                ));
                None
            }
        }
        SelectorChoice::Fresh(preset_name) => match write_preset(config, preset_name) {
            Ok(alias) => Some(alias),
            Err(msg) => {
                errors.push(QuickstartError::new(step, "", msg));
                None
            }
        },
    }
}

fn risk_preset_keys(config: &Config) -> Vec<String> {
    config.risk_profiles.keys().cloned().collect()
}

fn write_risk_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset =
        risk_preset(preset_name).ok_or_else(|| format!("unknown risk preset `{preset_name}`"))?;
    // Existing block wins — never clobber a user-customised `[risk-profiles.<name>]`
    // that happens to share a preset name.
    if config.risk_profiles.contains_key(preset.preset_name) {
        return Ok(preset.preset_name.to_string());
    }
    config
        .create_map_key("risk_profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .risk_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    config.mark_dirty(&format!("risk_profiles.{}", preset.preset_name));
    Ok(preset.preset_name.to_string())
}

fn write_runtime_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset = runtime_preset(preset_name)
        .ok_or_else(|| format!("unknown runtime preset `{preset_name}`"))?;
    // Existing block wins — same rule as `write_risk_preset`.
    if config.runtime_profiles.contains_key(preset.preset_name) {
        return Ok(preset.preset_name.to_string());
    }
    config
        .create_map_key("runtime_profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .runtime_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    config.mark_dirty(&format!("runtime_profiles.{}", preset.preset_name));
    Ok(preset.preset_name.to_string())
}

// ── Memory ─────────────────────────────────────────────────────────

fn apply_memory(
    config: &mut Config,
    choice: &SelectorChoice<MemoryChoice>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Memory,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                    return None;
                }
            };
            if !section_has_alias(config, "storage", family, alias) {
                let path = format!("storage.{family}.{alias}");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::Memory,
                    "",
                    format!("no `{path}` configured"),
                    "cli-quickstart-error-no-configured-path",
                    &[("path", &path)],
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent("memory.backend", reference) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "backend",
                    err.to_string(),
                ));
                return None;
            }
            Some(reference.clone())
        }
        SelectorChoice::Fresh(kind) => {
            // The schema's `MemoryBackendKind::serialize` rename
            // (`#[serde(rename_all = "snake_case")]`) gives us the
            // canonical TOML kebab-case spelling without any
            // surface-side mapping table. `None` writes `"none"`,
            // every other backend creates a `[storage.<kind>.<kind>]`
            // table and points `memory.backend` at it.
            let kind_name = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
            if matches!(kind, MemoryChoice::None) {
                if let Err(err) = config.set_prop_persistent("memory.backend", "none") {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Memory,
                        "backend",
                        err.to_string(),
                    ));
                    return None;
                }
                return Some("none".to_string());
            }
            let backend_ref = format!("{kind_name}.{kind_name}");
            let parent_path = format!("storage.{kind_name}");
            if let Err(err) = config.create_map_key(&parent_path, &kind_name) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "",
                    err.to_string(),
                ));
                return None;
            }
            if let Err(err) = config.set_prop_persistent("memory.backend", &backend_ref) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "backend",
                    err.to_string(),
                ));
                return None;
            }
            Some(backend_ref)
        }
    }
}

// ── Channels ───────────────────────────────────────────────────────

fn apply_channels(
    config: &mut Config,
    channels: &[SelectorChoice<ChannelQuickStart>],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Vec<String> {
    let mut refs = Vec::with_capacity(channels.len());
    for (idx, ch) in channels.iter().enumerate() {
        match ch {
            SelectorChoice::Existing(reference) => {
                if let Some((family, alias)) = split_ref(reference) {
                    if !channel_exists(config, family, alias) {
                        let path = format!("channels.{family}.{alias}");
                        errors.push(QuickstartError::for_surface(
                            ctx,
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            format!("no `{path}` configured"),
                            "cli-quickstart-error-no-configured-path",
                            &[("path", &path)],
                        ));
                        continue;
                    }
                    // Existing channel already bound to a different agent
                    // cannot be re-used — one channel, one agent invariant.
                    if let Some(owner) = config.agent_for_channel(reference) {
                        errors.push(QuickstartError::for_surface(
                            ctx,
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            format!("channel `{reference}` is already bound to agent `{owner}`"),
                            "cli-quickstart-error-channel-bound",
                            &[("reference", reference), ("owner", owner)],
                        ));
                        continue;
                    }
                    refs.push(reference.clone());
                } else {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                        "cli-quickstart-error-not-type-alias-ref",
                        &[("reference", reference)],
                    ));
                }
            }
            SelectorChoice::Fresh(entry) => {
                if entry.channel_type.trim().is_empty() || entry.alias.trim().is_empty() {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        "channel type and alias are required",
                        "cli-quickstart-error-channel-required",
                        &[],
                    ));
                    continue;
                }
                if channel_exists(config, &entry.channel_type, &entry.alias) {
                    let alias_ref = format!("{}.{}", entry.channel_type, entry.alias);
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].alias"),
                        format!("alias `{alias_ref}` already exists"),
                        "cli-quickstart-error-alias-exists",
                        &[("alias", &alias_ref)],
                    ));
                    continue;
                }
                if let Err(err) =
                    config.create_map_key(&format!("channels.{}", entry.channel_type), &entry.alias)
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].channel_type"),
                        err.to_string(),
                    ));
                    continue;
                }
                let token_path =
                    format!("channels.{}.{}.bot_token", entry.channel_type, entry.alias);
                if let Some(tok) = &entry.token {
                    if let Err(err) = config.set_prop_persistent(&token_path, tok) {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}].token"),
                            err.to_string(),
                        ));
                        continue;
                    }
                } else {
                    // No creds — still need to materialize the entry so the agent
                    // record can reference it. Set `enabled = true` as the minimum
                    // schema-recognised field; channels without creds will fail
                    // their own bootstrap loudly, which is the desired behaviour.
                    let enabled_path =
                        format!("channels.{}.{}.enabled", entry.channel_type, entry.alias);
                    if let Err(err) = config.set_prop_persistent(&enabled_path, "true") {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            err.to_string(),
                        ));
                        continue;
                    }
                }
                refs.push(format!("{}.{}", entry.channel_type, entry.alias));
            }
        }
    }
    refs
}

fn channel_exists(config: &Config, channel_type: &str, alias: &str) -> bool {
    let probe = format!("channels.{channel_type}.{alias}.enabled");
    config.get_prop(&probe).is_ok()
}

// ── Peer groups ────────────────────────────────────────────────────

fn apply_peer_groups(
    config: &mut Config,
    peer_groups: &[zeroclaw_config::presets::QuickstartPeerGroup],
    staged_channel_refs: &[String],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Vec<String> {
    let mut refs = Vec::with_capacity(peer_groups.len());
    for (idx, pg) in peer_groups.iter().enumerate() {
        if pg.name.trim().is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].name"),
                "peer-group name is required",
                "cli-quickstart-error-peer-group-name-required",
                &[],
            ));
            continue;
        }
        if pg.channel.trim().is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                "peer-group channel ref is required",
                "cli-quickstart-error-peer-group-channel-required",
                &[],
            ));
            continue;
        }
        // Channel ref must resolve to either a channel already in config
        // OR a channel staged in this same submission.
        let staged_match = staged_channel_refs.iter().any(|r| r == &pg.channel);
        let configured_match = match split_ref(&pg.channel) {
            Some((family, alias)) => channel_exists(config, family, alias),
            None => false,
        };
        if !staged_match && !configured_match {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                format!(
                    "peer-group `{}` references unknown channel `{}`",
                    pg.name, pg.channel
                ),
                "cli-quickstart-error-peer-group-unknown-channel",
                &[("name", &pg.name), ("channel", &pg.channel)],
            ));
            continue;
        }
        // Collision: existing peer-group block wins. Surface the conflict
        // so the operator sees what they need to rename.
        if config.peer_groups.contains_key(&pg.name) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].name"),
                format!("peer-group `{}` already exists", pg.name),
                "cli-quickstart-error-peer-group-exists",
                &[("name", &pg.name)],
            ));
            continue;
        }
        if let Err(err) = config.create_map_key("peer_groups", &pg.name) {
            errors.push(QuickstartError::new(
                QuickstartStep::Channels,
                format!("peer_groups[{idx}]"),
                err.to_string(),
            ));
            continue;
        }
        let prefix = format!("peer_groups.{}", pg.name);
        if let Err(err) = config.set_prop_persistent(&format!("{prefix}.channel"), &pg.channel) {
            errors.push(QuickstartError::new(
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                err.to_string(),
            ));
            continue;
        }
        if !pg.external_peers.is_empty() {
            let joined = pg
                .external_peers
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(err) =
                config.set_prop_persistent(&format!("{prefix}.external_peers"), &joined)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::Channels,
                    format!("peer_groups[{idx}].external_peers"),
                    err.to_string(),
                ));
                continue;
            }
        }
        if !pg.ignore.is_empty() {
            let joined = pg
                .ignore
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if let Err(err) = config.set_prop_persistent(&format!("{prefix}.ignore"), &joined) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Channels,
                    format!("peer_groups[{idx}].ignore"),
                    err.to_string(),
                ));
                continue;
            }
        }
        refs.push(pg.name.clone());
    }
    refs
}

// ── Personality files ──────────────────────────────────────────────

/// A personality file staged to a tempfile during `apply_into`, moved
/// into place only after the atomic config write succeeds. On config
/// failure the tempfile drops and cleans itself up — nothing orphaned.
struct StagedPersonalityWrite {
    tempfile: tempfile::NamedTempFile,
    dest: std::path::PathBuf,
}

fn apply_personality_files(
    config: &Config,
    agent_alias: &str,
    files: &[zeroclaw_config::presets::QuickstartPersonalityFile],
    staged: &mut Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) {
    if files.is_empty() {
        return;
    }
    let workspace = config.agent_workspace_dir(agent_alias);
    if let Err(err) = std::fs::create_dir_all(&workspace) {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "personality_files",
            format!("could not create agent workspace: {err}"),
            "cli-quickstart-error-personality-workspace",
            &[("err", &err.to_string())],
        ));
        return;
    }
    for (idx, file) in files.iter().enumerate() {
        let trimmed = file.filename.trim();
        if trimmed.is_empty() {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].filename"),
                "filename is required",
                "cli-quickstart-error-personality-filename-required",
                &[],
            ));
            continue;
        }
        if !crate::agent::personality::EDITABLE_PERSONALITY_FILES.contains(&trimmed) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].filename"),
                format!("`{trimmed}` is not an editable personality file"),
                "cli-quickstart-error-personality-not-editable",
                &[("filename", trimmed)],
            ));
            continue;
        }
        if file.content.chars().count() > crate::agent::personality::MAX_FILE_CHARS {
            let limit = crate::agent::personality::MAX_FILE_CHARS.to_string();
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}].content"),
                format!("content exceeds {} char limit", limit),
                "cli-quickstart-error-personality-too-large",
                &[("limit", &limit)],
            ));
            continue;
        }
        // Stage to a tempfile in the destination directory rather than
        // writing the final path now. The commit happens after the atomic
        // config persist in `apply_with_surface`.
        let mut tempfile = match tempfile::NamedTempFile::new_in(&workspace) {
            Ok(t) => t,
            Err(err) => {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::Agent,
                    format!("personality_files[{idx}]"),
                    format!("could not stage `{trimmed}`: {err}"),
                    "cli-quickstart-error-personality-stage-failed",
                    &[("filename", trimmed), ("err", &err.to_string())],
                ));
                continue;
            }
        };
        if let Err(err) = std::io::Write::write_all(&mut tempfile, file.content.as_bytes()) {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                format!("personality_files[{idx}]"),
                format!("could not stage `{trimmed}`: {err}"),
                "cli-quickstart-error-personality-stage-failed",
                &[("filename", trimmed), ("err", &err.to_string())],
            ));
            continue;
        }
        staged.push(StagedPersonalityWrite {
            tempfile,
            dest: workspace.join(trimmed),
        });
    }
}

/// Move every staged tempfile into place. Called only after the atomic
/// config write succeeds; a failure here is reported but the agent is
/// already persisted and valid.
fn commit_personality_files(
    staged: Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) {
    for write in staged {
        if let Err(err) = write.tempfile.persist(&write.dest) {
            let path = write.dest.display().to_string();
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Agent,
                "personality_files",
                format!("could not write `{path}`: {}", err.error),
                "cli-quickstart-error-personality-write-failed",
                &[("path", &path), ("err", &err.error.to_string())],
            ));
        }
    }
}

// ── Default skills bundle FTUE ─────────────────────────────────────

fn materialize_default_skills_bundle(config: &mut Config) {
    if !config.skill_bundles.is_empty() {
        return;
    }
    // create_map_key returns Ok(false) on existing key (idempotent),
    // Ok(true) on insertion. We don't propagate the error: the FTUE
    // bundle is best-effort and the operator can configure one later.
    let _ = config.create_map_key("skill-bundles", "default");
}

// ── Agent ──────────────────────────────────────────────────────────

fn apply_agent(
    config: &mut Config,
    identity: &AgentIdentity,
    provider_ref: &str,
    risk_alias: &str,
    runtime_alias: &str,
    channel_refs: &[String],
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<String> {
    if identity.name.trim().is_empty() {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "name",
            "agent name is required",
            "cli-quickstart-error-agent-name-required",
            &[],
        ));
        return None;
    }
    if config.agents.contains_key(&identity.name) {
        errors.push(QuickstartError::for_surface(
            ctx,
            QuickstartStep::Agent,
            "name",
            format!("agent `{}` already exists", identity.name),
            "cli-quickstart-error-agent-exists",
            &[("name", &identity.name)],
        ));
        return None;
    }

    let prefix = format!("agents.{}", identity.name);
    if let Err(err) = config.create_map_key("agents", &identity.name) {
        errors.push(QuickstartError::new(
            QuickstartStep::Agent,
            "name",
            err.to_string(),
        ));
        return None;
    }
    let writes: [(&str, &str); 3] = [
        ("model_provider", provider_ref),
        ("risk_profile", risk_alias),
        ("runtime_profile", runtime_alias),
    ];
    for (field, value) in writes {
        let path = format!("{prefix}.{field}");
        if let Err(err) = config.set_prop_persistent(&path, value) {
            errors.push(QuickstartError::new(
                QuickstartStep::Agent,
                field,
                err.to_string(),
            ));
            return None;
        }
    }
    if !channel_refs.is_empty() {
        let path = format!("{prefix}.channels");
        let json = serde_json::to_string(channel_refs).unwrap_or_else(|_| "[]".to_string());
        if let Err(err) = config.set_prop_persistent(&path, &json) {
            errors.push(QuickstartError::new(
                QuickstartStep::Agent,
                "channels",
                err.to_string(),
            ));
            return None;
        }
    }
    Some(identity.name.clone())
}

// ── Shared helpers ─────────────────────────────────────────────────

fn split_ref(reference: &str) -> Option<(&str, &str)> {
    let (ty, alias) = reference.split_once('.')?;
    if ty.is_empty() || alias.is_empty() {
        None
    } else {
        Some((ty, alias))
    }
}

/// Probe whether `<prefix>.<family>.<alias>` resolves to a populated
/// entry. Uses the schema's own `get_prop` dispatch — no per-family
/// list. We probe a path the entry's own struct must have if it
/// exists (`enabled` or `model`); the schema bubbles an error for
/// unknown families which we treat as "not present".
fn section_has_alias(config: &Config, prefix: &str, family: &str, alias: &str) -> bool {
    for probe_field in ["enabled", "model", "uri"] {
        let probe = format!("{prefix}.{family}.{alias}.{probe_field}");
        if config.get_prop(&probe).is_ok() {
            return true;
        }
    }
    false
}

/// Live model catalog for a provider type. `(models, pricing, live)`:
/// `live=true` means surfaces should render a picker; `live=false`
/// means fall back to free text. Tries `ModelProvider::list_models_with_pricing()`
/// first, then the family catalog table (no pricing for fallbacks).
pub async fn model_catalog(
    model_provider: &str,
) -> (
    Vec<String>,
    Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
    bool,
) {
    if let Ok(handle) = zeroclaw_providers::create_model_provider(model_provider, None)
        && let Ok(models) = zeroclaw_providers::ProviderDispatch::from_ref(&*handle)
            .list_models_with_pricing()
            .await
        && !models.is_empty()
    {
        let raw_pricing: std::collections::HashMap<
            String,
            zeroclaw_api::model_provider::ModelPricing,
        > = models
            .iter()
            .filter_map(|m| m.pricing.as_ref().map(|p| (m.id.clone(), p.clone())))
            .collect();
        let ids = models.into_iter().map(|m| m.id).collect();
        let Some(ids) =
            zeroclaw_providers::catalog::sort_model_catalog_for_chat(model_provider, ids)
        else {
            return (Vec::new(), None, false);
        };
        let pricing: std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing> =
            ids.iter()
                .filter_map(|id| raw_pricing.get(id).map(|p| (id.clone(), p.clone())))
                .collect();
        let pricing = if pricing.is_empty() {
            None
        } else {
            Some(pricing)
        };
        return (ids, pricing, true);
    }
    match zeroclaw_providers::catalog::list_models_for_family(model_provider).await {
        Ok(models) if !models.is_empty() => (
            zeroclaw_providers::catalog::sort_model_catalog_for_chat(model_provider, models)
                .unwrap_or_default(),
            None,
            true,
        ),
        _ => (Vec::new(), None, false),
    }
}

/// `true` for model_provider families that need no remote credential.
#[must_use]
pub fn model_provider_is_local(model_provider: &str) -> bool {
    zeroclaw_providers::list_model_providers()
        .iter()
        .find(|p| p.name == model_provider)
        .is_some_and(|p| p.local)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
        SelectorChoice,
    };
    use zeroclaw_config::schema::Config;

    /// Regression: every channel kind the schema enumerates in
    /// `ChannelsConfig::channels()` must appear in the Quickstart
    /// `channel_types` picker. The previous implementation walked the
    /// serialized form of `ChannelsConfig`, which hid every empty
    /// channel HashMap because of
    /// `#[serde(skip_serializing_if = "HashMap::is_empty")]` — that
    /// silently truncated the picker to whatever channels happened
    /// to have a configured alias on the live config (~9 instead of
    /// 32). Drive the picker from the schema's curated list so the
    /// picker matches what the schema knows about.
    #[test]
    fn channel_type_options_cover_every_schema_channel() {
        let cfg = Config::default();
        let picker = build_channel_type_options(&cfg.channels);
        let schema = cfg.channels.channels();
        assert_eq!(
            picker.len(),
            schema.len(),
            "Quickstart channel-type picker count diverged from \
             ChannelsConfig::channels(); picker has {} rows, schema has {}",
            picker.len(),
            schema.len(),
        );
        for (picked, expected) in picker.iter().zip(schema.iter()) {
            assert_eq!(
                picked.kind, expected.kind,
                "kind mismatch at {} — picker `{}`, schema `{}`",
                picked.display_name, picked.kind, expected.kind,
            );
            assert_eq!(
                picked.display_name, expected.name,
                "display_name mismatch at `{}` — picker `{}`, schema `{}`",
                picked.kind, picked.display_name, expected.name,
            );
        }
    }

    fn fresh_submission(agent_name: &str) -> BuilderSubmission {
        BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                model: "claude-sonnet-4-5".into(),
                fields: std::collections::HashMap::from([(
                    "api_key".to_string(),
                    "sk-test".to_string(),
                )]),
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            peer_groups: vec![],
            agent: AgentIdentity {
                name: agent_name.into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
                personality_files: vec![],
            },
        }
    }

    #[test]
    fn apply_serializes_provider_fields_as_snake_case() {
        let mut cfg = Config::default();
        let submission = fresh_submission("bot");
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some(), "apply_into should yield an agent");
        // The submission carries the snake field key `api_key` and it must
        // land on disk as the snake serde field `api_key`, never kebab.
        let toml = toml::to_string(&cfg).expect("serialize config");
        assert!(
            toml.contains("api_key"),
            "expected snake `api_key` in serialized config:\n{toml}"
        );
        assert!(
            !toml.contains("api-key"),
            "kebab `api-key` leaked into serialized config:\n{toml}"
        );
    }

    #[test]
    fn apply_provider_type_trims_and_canonicalizes_whitespace() {
        // A provider type with stray whitespace must canonicalize to the
        // registry's family key, not reach create_map_key verbatim (which would
        // fail with "no map-keyed/list section at providers.models.llamacpp ").
        let mut cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.model_provider = SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "  llamacpp  ".into(),
            alias: "local".into(),
            model: "qwen2.5-coder".into(),
            fields: std::collections::HashMap::new(),
        });
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        assert!(
            cfg.providers.models.find("llamacpp", "local").is_some(),
            "expected providers.models.llamacpp.local to exist"
        );
        let agent = cfg.agents.get("bot").expect("agent created");
        assert_eq!(agent.model_provider.as_str(), "llamacpp.local");
    }

    #[test]
    fn apply_provider_type_case_insensitive() {
        let mut cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.model_provider = SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "Anthropic".into(),
            alias: "main".into(),
            model: "claude-sonnet-4-5".into(),
            fields: std::collections::HashMap::new(),
        });
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        assert!(errors.is_empty(), "apply_into errors: {errors:?}");
        assert!(applied.is_some());
        assert!(cfg.providers.models.find("anthropic", "main").is_some());
    }

    #[test]
    fn apply_unknown_provider_type_errors_clearly() {
        let mut cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.model_provider = SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: "not_a_real_provider".into(),
            alias: "x".into(),
            model: "m".into(),
            fields: std::collections::HashMap::new(),
        });
        let mut staged = Vec::new();
        let mut errors = Vec::new();
        let applied = apply_into(&mut cfg, &submission, &mut staged, &mut errors, None);
        assert!(applied.is_none());
        assert!(
            errors
                .iter()
                .any(|e| e.step == QuickstartStep::ModelProvider
                    && e.message.contains("unknown model provider type")),
            "expected a clear unknown-provider error, got: {errors:?}"
        );
    }

    #[test]
    fn validate_only_passes_on_fresh_submission() {
        let cfg = Config::default();
        let submission = fresh_submission("bot");
        validate_only(&submission, &cfg).expect("fresh submission validates");
    }

    #[test]
    fn validate_only_rejects_blank_agent_name() {
        let cfg = Config::default();
        let submission = fresh_submission("");
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(
            errors
                .iter()
                .any(|e| e.step == QuickstartStep::Agent && e.field == "name")
        );
    }

    #[test]
    fn validate_only_rejects_existing_agent_name() {
        let mut cfg = Config::default();
        cfg.agents.insert(
            "bot".into(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );
        let submission = fresh_submission("bot");
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(errors.iter().any(|e| e.step == QuickstartStep::Agent));
    }

    #[test]
    fn validate_only_rejects_unknown_risk_preset() {
        let cfg = Config::default();
        let mut submission = fresh_submission("bot");
        submission.risk_profile = SelectorChoice::Fresh("does-not-exist".into());
        let errors = validate_only(&submission, &cfg).unwrap_err();
        assert!(errors.iter().any(|e| e.step == QuickstartStep::RiskProfile));
    }

    #[test]
    fn validate_only_accepts_every_builtin_risk_preset() {
        let cfg = Config::default();
        for p in zeroclaw_config::presets::RISK_PRESETS {
            let mut submission = fresh_submission("bot");
            submission.risk_profile = SelectorChoice::Fresh(p.preset_name.into());
            validate_only(&submission, &cfg).unwrap_or_else(|e| {
                panic!("risk preset `{}` failed validate: {e:?}", p.preset_name)
            });
        }
    }

    /// Regression for the silent empty-form bug: `field_shape(ModelProvider,
    /// <type>)` must return at least the model + api-key rows for every
    /// known model provider type. Before fix, the synthetic probe alias
    /// failed `validate_alias_key`, the recurse arms in the Configurable
    /// derive masked it as "no map-keyed/list section", and field_shape
    /// silently returned an empty Vec — leaving the TUI form with zero
    /// editable rows and the CLI wizard dumped to a manual `Model id for X:`
    /// fallback.
    #[test]
    fn field_shape_returns_model_provider_rows_for_canonical_types() {
        for kind in ["anthropic", "openai", "ollama", "openrouter", "groq"] {
            let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
            let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
            assert!(
                keys.contains(&"model"),
                "field_shape for `{kind}` is missing `model` row; got {keys:?}",
            );
            assert!(
                keys.contains(&"api_key"),
                "field_shape for `{kind}` is missing `api_key` row; got {keys:?}",
            );
        }
    }

    /// Codex subscription auth: `field_shape(ModelProvider, "openai")` must
    /// include the `requires_openai_auth` and `wire_api` rows so the
    /// Quickstart form can offer Codex subscription auth (no API key needed).
    /// These fields are non-required — they default to `false`/empty and are
    /// harmless for non-OpenAI providers.
    #[test]
    fn field_shape_openai_includes_codex_auth_fields() {
        let rows = super::field_shape(super::FieldSection::ModelProvider, "openai");
        let keys: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert!(
            keys.contains(&"requires_openai_auth"),
            "field_shape for openai must include `requires_openai_auth` for Codex subscription; got {keys:?}",
        );
        assert!(
            keys.contains(&"wire_api"),
            "field_shape for openai must include `wire_api` for Codex subscription; got {keys:?}",
        );
        // Both must be non-required so Quickstart doesn't block on them.
        for row in &rows {
            if row.key == "requires_openai_auth" || row.key == "wire_api" {
                assert!(
                    !row.required,
                    "`{}` must be non-required in the Quickstart form",
                    row.key
                );
            }
        }
        // No row may carry the `<unset>` placeholder as its default.
        // It's a display sentinel for an unset Option; echoing it back
        // through any surface (CLI/TUI/web) makes the daemon validate
        // `<unset>` against the field's real type and reject it.
        for row in &rows {
            assert_ne!(
                row.default.as_deref(),
                Some(zeroclaw_config::traits::UNSET_DISPLAY),
                "`{}` must not default to the <unset> placeholder",
                row.key
            );
        }
    }

    /// `api_key` must be non-required in the Quickstart form so Codex
    /// subscription (no API key) and local providers (Ollama) can proceed
    /// without one.
    #[test]
    fn field_shape_api_key_is_not_required() {
        for kind in ["openai", "ollama"] {
            let rows = super::field_shape(super::FieldSection::ModelProvider, kind);
            let api_key_row = rows.iter().find(|r| r.key == "api_key");
            assert!(
                api_key_row.is_some(),
                "field_shape for `{kind}` must include `api_key`",
            );
            assert!(
                !api_key_row.unwrap().required,
                "`api_key` must be non-required for `{kind}` (Codex subscription / local providers don't need one)",
            );
        }
    }

    async fn apply_to_temp(submission: BuilderSubmission) -> (tempfile::TempDir, Config) {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            config_path: dir.path().join("config.toml"),
            data_dir: dir.path().join("data"),
            ..Default::default()
        };
        config.save().await.unwrap();
        let mut config = config;
        super::apply(submission, &mut config)
            .await
            .expect("apply should succeed");
        (dir, config)
    }

    fn reload(dir: &tempfile::TempDir) -> Config {
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        toml::from_str(&raw).expect("on-disk config must round-trip")
    }

    #[tokio::test]
    async fn fresh_preset_profiles_persist_to_disk() {
        // The runtime profile picker was removed; apply silently forces the
        // `unbounded` preset regardless of the submitted runtime value.
        let (dir, applied) = apply_to_temp(fresh_submission("bot")).await;
        assert!(applied.risk_profiles.contains_key("balanced"));
        assert!(applied.runtime_profiles.contains_key("unbounded"));
        let reloaded = reload(&dir);
        assert!(
            reloaded.risk_profiles.contains_key("balanced"),
            "risk_profiles.balanced must survive save_dirty + reload, not dangle"
        );
        assert!(
            reloaded.runtime_profiles.contains_key("unbounded"),
            "runtime_profiles.unbounded must survive save_dirty + reload, not dangle"
        );
        let agent = reloaded.agents.get("bot").expect("agent persisted");
        assert_eq!(agent.risk_profile, "balanced");
        assert_eq!(agent.runtime_profile, "unbounded");
    }

    #[tokio::test]
    async fn multiple_channels_all_bind_to_agent() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![
            SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: "telegram".into(),
                alias: "tg".into(),
                token: Some("tok-a".into()),
            }),
            SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: "discord".into(),
                alias: "dc".into(),
                token: Some("tok-b".into()),
            }),
        ];
        let (dir, _applied) = apply_to_temp(submission).await;
        let reloaded = reload(&dir);
        let agent = reloaded.agents.get("bot").expect("agent persisted");
        let bound: Vec<String> = agent.channels.iter().map(|c| c.to_string()).collect();
        assert!(
            bound.iter().any(|c| c.contains("tg")),
            "first channel must stay bound; got {bound:?}"
        );
        assert!(
            bound.iter().any(|c| c.contains("dc")),
            "second channel must also be bound; got {bound:?}"
        );
        assert_eq!(bound.len(), 2, "both channels bound, not just the last");
    }

    #[tokio::test]
    async fn peer_groups_persist_to_canonical_section() {
        let mut submission = fresh_submission("bot");
        submission.channels = vec![SelectorChoice::Fresh(ChannelQuickStart {
            channel_type: "telegram".into(),
            alias: "tg".into(),
            token: Some("tok-a".into()),
        })];
        submission.peer_groups = vec![zeroclaw_config::presets::QuickstartPeerGroup {
            name: "team".into(),
            channel: "telegram.tg".into(),
            external_peers: vec!["*".into()],
            ignore: vec![],
        }];

        let (dir, _applied) = apply_to_temp(submission).await;
        let raw = std::fs::read_to_string(dir.path().join("config.toml")).unwrap();
        assert!(
            raw.contains("[peer_groups.team]"),
            "Quickstart must serialize peer groups through canonical snake_case paths:\n{raw}"
        );
        assert!(
            !raw.contains("[peer-groups.team]"),
            "Quickstart must not write the stale kebab-case peer-groups path:\n{raw}"
        );

        let reloaded = reload(&dir);
        let group = reloaded
            .peer_groups
            .get("team")
            .expect("peer group persisted");
        assert_eq!(group.channel, "telegram.tg");
        assert_eq!(group.external_peers, vec!["*".to_string()]);
    }
}
