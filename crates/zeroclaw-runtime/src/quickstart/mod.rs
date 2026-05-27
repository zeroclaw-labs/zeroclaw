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
    Agent,
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
    apply_into(&mut staged, submission, &mut errors, Some(&ctx));
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
    let applied = apply_into(config, &submission, &mut errors, Some(&ctx));
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
    let applied = applied.expect("apply_into yields Some when errors is empty");

    config
        .set_prop_persistent("onboard-state.quickstart-completed", "true")
        .map_err(|err| {
            vec![QuickstartError::new(
                QuickstartStep::Agent,
                "",
                format!("failed to flip quickstart-completed: {err}"),
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
        vec![QuickstartError::new(
            QuickstartStep::Agent,
            "",
            format!("failed to persist config: {err}"),
        )]
    })?;

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
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
    /// `<storage_type>.<alias>` refs.
    pub storage: Vec<String>,
}

/// Build a [`QuickstartState`] snapshot from the live config.
pub fn snapshot_state(cfg: &Config) -> QuickstartState {
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
        storage: collect_aliased_refs(&cfg.storage),
    }
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
    const SYNTHETIC_ALIAS: &str = "__qs_shape__";
    let (section_path, essentials) = match section {
        FieldSection::ModelProvider => (
            format!("providers.models.{type_key}"),
            MODEL_PROVIDER_ESSENTIALS,
        ),
        FieldSection::Channel => (format!("channels.{type_key}"), CHANNEL_ESSENTIALS),
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
        // ghost-text defaults for plain fields only.
        let default = if info.is_secret {
            None
        } else {
            let raw = info.display_value.trim();
            if raw.is_empty() {
                None
            } else {
                Some(raw.to_string())
            }
        };
        out.push(FieldDescriptor {
            key: field_path.to_string(),
            label: humanize_field_key(field_path),
            help: info.description.trim().to_string(),
            kind: info.kind,
            is_secret: info.is_secret,
            enum_variants: info.enum_variants.map(|f| f()),
            required: true,
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
const MODEL_PROVIDER_ESSENTIALS: &[&str] = &["model", "api-key", "base-url"];
const CHANNEL_ESSENTIALS: &[&str] = &["bot-token", "token", "webhook-url", "allowed-users"];

fn humanize_field_key(key: &str) -> String {
    let mut s = key.replace('-', " ");
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

fn apply_into(
    config: &mut Config,
    submission: &BuilderSubmission,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<AppliedAgent> {
    let provider_ref = apply_model_provider(config, &submission.model_provider, errors)?;
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
    )?;
    emit_selector_pick(
        ctx,
        "risk_profile",
        selector_mode(&submission.risk_profile),
        &risk_alias,
    );

    let runtime_alias = apply_named_preset(
        config,
        &submission.runtime_profile,
        QuickstartStep::RuntimeProfile,
        runtime_preset_keys,
        write_runtime_preset,
        errors,
    )?;
    emit_selector_pick(
        ctx,
        "runtime_profile",
        selector_mode(&submission.runtime_profile),
        &runtime_alias,
    );

    let memory_backend = apply_memory(config, &submission.memory, errors)?;
    emit_selector_pick(
        ctx,
        "memory",
        selector_mode(&submission.memory),
        &memory_backend,
    );

    let channel_refs = apply_channels(config, &submission.channels, errors);
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
    )?;
    emit_selector_pick(ctx, "agent", "create_new", &alias);

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
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                    ));
                    return None;
                }
            };
            if !section_has_alias(config, "providers.models", family, alias) {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "",
                    format!("no `providers.models.{family}.{alias}` configured"),
                ));
                return None;
            }
            Some(reference.clone())
        }
        SelectorChoice::Fresh(choice) => {
            if choice.provider_type.trim().is_empty()
                || choice.alias.trim().is_empty()
                || choice.default_model.trim().is_empty()
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "",
                    "provider type, alias, and default model are required",
                ));
                return None;
            }
            if section_has_alias(
                config,
                "providers.models",
                &choice.provider_type,
                &choice.alias,
            ) {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "alias",
                    format!(
                        "alias `{}.{}` already exists",
                        choice.provider_type, choice.alias
                    ),
                ));
                return None;
            }
            let prefix = format!("providers.models.{}.{}", choice.provider_type, choice.alias);
            if let Err(err) = config.create_map_key(
                &format!("providers.models.{}", choice.provider_type),
                &choice.alias,
            ) {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "provider_type",
                    err.to_string(),
                ));
                return None;
            }
            if let Err(err) =
                config.set_prop_persistent(&format!("{prefix}.model"), &choice.default_model)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "default_model",
                    err.to_string(),
                ));
                return None;
            }
            if let Some(key) = &choice.api_key
                && let Err(err) = config.set_prop_persistent(&format!("{prefix}.api-key"), key)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "api_key",
                    err.to_string(),
                ));
                return None;
            }
            if let Some(uri) = &choice.base_url
                && let Err(err) = config.set_prop_persistent(&format!("{prefix}.uri"), uri)
            {
                errors.push(QuickstartError::new(
                    QuickstartStep::ModelProvider,
                    "base_url",
                    err.to_string(),
                ));
                return None;
            }
            Some(format!("{}.{}", choice.provider_type, choice.alias))
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
                errors.push(QuickstartError::new(
                    step,
                    "",
                    format!("no `{alias}` profile configured"),
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

fn runtime_preset_keys(config: &Config) -> Vec<String> {
    config.runtime_profiles.keys().cloned().collect()
}

fn write_risk_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset =
        risk_preset(preset_name).ok_or_else(|| format!("unknown risk preset `{preset_name}`"))?;
    config
        .create_map_key("risk-profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .risk_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    Ok(preset.preset_name.to_string())
}

fn write_runtime_preset(config: &mut Config, preset_name: &str) -> Result<String, String> {
    let preset = runtime_preset(preset_name)
        .ok_or_else(|| format!("unknown runtime preset `{preset_name}`"))?;
    config
        .create_map_key("runtime-profiles", preset.preset_name)
        .map_err(|e| e.to_string())?;
    config
        .runtime_profiles
        .insert(preset.preset_name.to_string(), (preset.values)());
    Ok(preset.preset_name.to_string())
}

// ── Memory ─────────────────────────────────────────────────────────

fn apply_memory(
    config: &mut Config,
    choice: &SelectorChoice<MemoryChoice>,
    errors: &mut Vec<QuickstartError>,
) -> Option<String> {
    match choice {
        SelectorChoice::Existing(reference) => {
            let (family, alias) = match split_ref(reference) {
                Some(parts) => parts,
                None => {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Memory,
                        "",
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                    ));
                    return None;
                }
            };
            if !section_has_alias(config, "storage", family, alias) {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "",
                    format!("no `storage.{family}.{alias}` configured"),
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
        SelectorChoice::Fresh(MemoryChoice::Sqlite) => {
            let backend_ref = "sqlite.sqlite".to_string();
            if let Err(err) = config.create_map_key("storage.sqlite", "sqlite") {
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
        SelectorChoice::Fresh(MemoryChoice::None) => {
            if let Err(err) = config.set_prop_persistent("memory.backend", "none") {
                errors.push(QuickstartError::new(
                    QuickstartStep::Memory,
                    "backend",
                    err.to_string(),
                ));
                return None;
            }
            Some("none".to_string())
        }
    }
}

// ── Channels ───────────────────────────────────────────────────────

fn apply_channels(
    config: &mut Config,
    channels: &[SelectorChoice<ChannelQuickStart>],
    errors: &mut Vec<QuickstartError>,
) -> Vec<String> {
    let mut refs = Vec::with_capacity(channels.len());
    for (idx, ch) in channels.iter().enumerate() {
        match ch {
            SelectorChoice::Existing(reference) => {
                if let Some((family, alias)) = split_ref(reference) {
                    if !channel_exists(config, family, alias) {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}]"),
                            format!("no `channels.{family}.{alias}` configured"),
                        ));
                        continue;
                    }
                    refs.push(reference.clone());
                } else {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        format!("`{reference}` is not a `<type>.<alias>` reference"),
                    ));
                }
            }
            SelectorChoice::Fresh(entry) => {
                if entry.channel_type.trim().is_empty() || entry.alias.trim().is_empty() {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}]"),
                        "channel type and alias are required",
                    ));
                    continue;
                }
                if channel_exists(config, &entry.channel_type, &entry.alias) {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].alias"),
                        format!(
                            "alias `{}.{}` already exists",
                            entry.channel_type, entry.alias
                        ),
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
                    format!("channels.{}.{}.bot-token", entry.channel_type, entry.alias);
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

// ── Agent ──────────────────────────────────────────────────────────

fn apply_agent(
    config: &mut Config,
    identity: &AgentIdentity,
    provider_ref: &str,
    risk_alias: &str,
    runtime_alias: &str,
    channel_refs: &[String],
    errors: &mut Vec<QuickstartError>,
) -> Option<String> {
    if identity.name.trim().is_empty() {
        errors.push(QuickstartError::new(
            QuickstartStep::Agent,
            "name",
            "agent name is required",
        ));
        return None;
    }
    if config.agents.contains_key(&identity.name) {
        errors.push(QuickstartError::new(
            QuickstartStep::Agent,
            "name",
            format!("agent `{}` already exists", identity.name),
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
        ("model-provider", provider_ref),
        ("risk-profile", risk_alias),
        ("runtime-profile", runtime_alias),
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
    for r in channel_refs {
        let path = format!("{prefix}.channels");
        if let Err(err) = config.set_prop_persistent(&path, r) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, MemoryChoice, ModelProviderChoice, SelectorChoice,
    };

    fn fresh_submission(agent_name: &str) -> BuilderSubmission {
        BuilderSubmission {
            model_provider: SelectorChoice::Fresh(ModelProviderChoice {
                provider_type: "anthropic".into(),
                alias: "anthropic".into(),
                default_model: "claude-sonnet-4-5".into(),
                api_key: Some("sk-test".into()),
                base_url: None,
            }),
            risk_profile: SelectorChoice::Fresh("balanced".into()),
            runtime_profile: SelectorChoice::Fresh("balanced".into()),
            memory: SelectorChoice::Fresh(MemoryChoice::Sqlite),
            channels: vec![],
            agent: AgentIdentity {
                name: agent_name.into(),
                system_prompt: "You are helpful.".into(),
                personality_file: None,
            },
        }
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
}
