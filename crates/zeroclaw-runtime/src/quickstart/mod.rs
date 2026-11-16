//! Quickstart apply path.

pub mod presets;

use serde::{Deserialize, Serialize};

use self::presets::{recommended_runtime_preset, risk_preset, runtime_preset};
use zeroclaw_config::helpers::kebab_to_snake;
use zeroclaw_config::presets::{
    AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
    SelectorChoice,
};
use zeroclaw_config::schema::{Config, WireApi};

/// Which surface invoked the Quickstart. Stamped on every event in
/// the apply path so SSE/dashboard consumers can filter by origin
/// without parsing message strings.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Web,
    Cli,
    Test,
}

impl Surface {
    pub fn as_str(self) -> &'static str {
        match self {
            Surface::Web => "web",
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

/// `onboard_state.quickstart_completed` is false **and** no
/// `agents.*` entries exist. Returning users with existing agents
/// never see the auto-trigger even if the flag was never flipped.
pub fn should_auto_launch(config: &Config) -> bool {
    !config.onboard_state.quickstart_completed && config.agents.is_empty()
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub struct QuickstartState {
    pub quickstart_completed: bool,
    pub agents: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    /// Canonical runtime fallback used when a provider has no recommendation.
    pub default_runtime_profile: String,
    /// `<provider_type>.<alias>` refs for every configured model provider.
    pub model_providers: Vec<String>,
    /// `<channel_type>.<alias>` refs.
    pub channels: Vec<String>,
    #[serde(default)]
    pub unassigned_channels: Vec<String>,
    /// `<storage_type>.<alias>` refs.
    pub storage: Vec<String>,
    pub model_provider_types: Vec<QuickstartTypeOption>,
    pub channel_types: Vec<QuickstartTypeOption>,
    /// Risk presets from `presets::RISK_PRESETS`.
    pub risk_presets: &'static [presets::RiskPreset],
    /// Runtime presets from `presets::RUNTIME_PRESETS`.
    pub runtime_presets: &'static [presets::RuntimePreset],
    /// Memory backend snake-case kinds from `MemoryBackendKind`.
    pub memory_kinds: Vec<String>,
    /// Canonical personality filenames the Quickstart will accept.
    /// Surfaces iterate this; never hardcode the filename list.
    pub personality_files: &'static [&'static str],
}

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
    /// Runtime preset the daemon recommends when this provider is selected.
    /// Resolved from provider registry metadata and the canonical preset table
    /// for each state snapshot; never persisted as config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runtime_profile: Option<String>,
}

/// Resolve a Quickstart provider-type input to the canonical config family.
///
/// The provider registry remains the source of truth for real config families.
/// Auth-provider aliases (`openai-codex`, `claude`, `google`, `grok`, ...)
/// are accepted as Quickstart conveniences and normalize to existing config
/// families. Only `openai-codex` preselects a different Quickstart auth mode.
#[must_use]
pub fn resolve_model_provider_type(type_key: &str) -> Option<(&'static str, bool)> {
    let trimmed = type_key.trim();
    if let Some(info) = zeroclaw_providers::list_model_providers()
        .into_iter()
        .find(|info| info.name.eq_ignore_ascii_case(trimmed))
    {
        return Some((info.name, false));
    }

    let provider = trimmed
        .parse::<zeroclaw_providers::auth::AuthProvider>()
        .ok()?;
    Some(match provider {
        zeroclaw_providers::auth::AuthProvider::OpenaiCodex => ("openai", true),
        zeroclaw_providers::auth::AuthProvider::Anthropic => ("anthropic", false),
        zeroclaw_providers::auth::AuthProvider::Gemini => ("gemini", false),
        zeroclaw_providers::auth::AuthProvider::Xai => ("xai", false),
    })
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
            default_runtime_profile: presets::recommended_runtime_profile(info.name)
                .and_then(runtime_preset)
                .map(|preset| preset.preset_name.to_string()),
        })
        .collect();
    let channel_types = build_channel_type_options(&cfg.channels);
    QuickstartState {
        quickstart_completed: cfg.onboard_state.quickstart_completed,
        agents: cfg.agents.keys().cloned().collect(),
        risk_profiles: cfg.risk_profiles.keys().cloned().collect(),
        runtime_profiles: cfg.runtime_profiles.keys().cloned().collect(),
        default_runtime_profile: recommended_runtime_preset(None)
            .map(|preset| preset.preset_name.to_string())
            .unwrap_or_default(),
        model_providers: cfg
            .providers
            .models
            .iter_entries()
            .map(|(family, alias, _)| format!("{family}.{alias}"))
            .collect(),
        // Retired transports never surface as assignable channels even when
        // their sections still parse (#584).
        channels: live_channel_refs(cfg),
        unassigned_channels: live_channel_refs(cfg)
            .into_iter()
            .filter(|ch| cfg.agent_for_channel(ch).is_none())
            .collect(),
        storage: collect_aliased_refs(&cfg.storage),
        model_provider_types,
        channel_types,
        risk_presets: presets::RISK_PRESETS,
        runtime_presets: presets::RUNTIME_PRESETS,
        memory_kinds: memory_kind_keys(),
        personality_files: crate::agent::personality::EDITABLE_PERSONALITY_FILES,
    }
}

/// Snake-case wire keys for available `MemoryBackendKind` variants. Exhaustive
/// match probe catches missing variants at compile time; serde produces
/// the wire key so there's no parallel mapping.
fn memory_kind_keys() -> Vec<String> {
    use zeroclaw_config::multi_agent::MemoryBackendKind as M;
    [M::Tachi, M::Markdown, M::None]
        .into_iter()
        .filter_map(|k| {
            // Exhaustiveness guard: adding a new variant forces this match to fail
            // to compile until the contributor decides whether the new backend
            // belongs in the quickstart picker. Retired variants only parse for
            // legacy configs and are never offered; tachi is offered only when
            // this build includes `memory-tachi`.
            match k {
                M::Markdown | M::None => (),
                M::Tachi if zeroclaw_memory::TACHI_COMPILED => (),
                M::Tachi | M::Sqlite | M::Lucid | M::Postgres | M::Qdrant => return None,
            }
            serde_json::to_value(k)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .collect()
}

fn build_channel_type_options(
    channels_cfg: &zeroclaw_config::schema::ChannelsConfig,
) -> Vec<QuickstartTypeOption> {
    // First-run setup may only offer live surfaces: the Telegram bridge,
    // webhook ingress, and the Email/Discord tool configuration. Retired
    // transports (no runner since #579/#581) must not be offered even
    // though their config sections still parse (#584).
    channels_cfg
        .channels()
        .into_iter()
        .filter(|info| !info.retired)
        .map(|info| QuickstartTypeOption {
            kind: info.kind.to_string(),
            display_name: info.name.to_string(),
            local: false,
            default_runtime_profile: None,
        })
        .collect()
}

fn canonical_quickstart_channel_ref(reference: &str) -> Option<String> {
    let (channel_type, alias) = split_ref(reference.trim())?;
    Some(format!("{channel_type}.{alias}"))
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

/// Configured channel refs excluding retired transports: quickstart may
/// only offer live surfaces for assignment (#584). Only the leading family
/// segment names a transport; alias segments are operator-chosen.
fn live_channel_refs(cfg: &zeroclaw_config::schema::Config) -> Vec<String> {
    collect_aliased_refs(&cfg.channels)
        .into_iter()
        .filter(|r| {
            r.split('.')
                .next()
                .is_none_or(|family| !zeroclaw_config::schema::is_retired_transport(family))
        })
        .collect()
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
    pub kind: zeroclaw_api::config_metadata::PropKind,
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
    const SYNTHETIC_ALIAS: &str = "qs0probe";
    let (section_path, essentials, codex_auth_preselected) = match section {
        FieldSection::ModelProvider => {
            let Some((provider_type, codex_auth_preselected)) =
                resolve_model_provider_type(type_key)
            else {
                return Vec::new();
            };
            (
                format!("providers.models.{provider_type}"),
                MODEL_PROVIDER_ESSENTIALS,
                codex_auth_preselected,
            )
        }
        FieldSection::Channel => (format!("channels.{type_key}"), CHANNEL_ESSENTIALS, false),
        FieldSection::PeerGroup => ("peer_groups".to_string(), PEER_GROUP_ESSENTIALS, false),
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
        let help = if section_path == "providers.models.anthropic" && field_path == "api_key" {
            crate::i18n::get_required_cli_string("cli-quickstart-anthropic-api-key-help")
        } else {
            info.description.trim().to_string()
        };
        out.push(FieldDescriptor {
            key: field_path.to_string(),
            label: kebab_to_snake(field_path),
            help,
            kind: info.kind,
            is_secret: info.is_secret,
            enum_variants: info.enum_variants.map(|f| f()),
            // `uri` is an override-only field — operators set it only
            // when pointing at a self-hosted gateway. `api_key` is left
            // non-required because local providers (Ollama) and Codex
            // subscription auth don't need one — the runtime surfaces a clear
            // error at request time if a remote provider is missing its key.
            // Everything else in the essentials list is required to actually
            // issue a request.
            required: !matches!(field_path, "uri" | "api_key"),
            default,
        });
    }
    if matches!(section, FieldSection::ModelProvider)
        && let Some(provider_type) = section_path.strip_prefix("providers.models.")
        && let Some(descriptor) = auth_mode_descriptor(provider_type, codex_auth_preselected)
    {
        out.push(descriptor);
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
const MODEL_PROVIDER_ESSENTIALS: &[&str] = &["model", QUICKSTART_AUTH_MODE_FIELD, "api_key", "uri"];
const CHANNEL_ESSENTIALS: &[&str] = &["bot_token", "token", "webhook_url", "allowed_users"];
const PEER_GROUP_ESSENTIALS: &[&str] = &["channel", "external_peers", "agents", "ignore"];

const QUICKSTART_AUTH_MODE_FIELD: &str = "auth_mode";
const QUICKSTART_AUTH_MODE_API_KEY: &str = "api_key";
const QUICKSTART_AUTH_MODE_CODEX: &str = "codex";
const QUICKSTART_AUTH_MODE_SETUP_TOKEN: &str = "setup_token";
const QUICKSTART_OPENAI_AUTH_MODES: &[&str] =
    &[QUICKSTART_AUTH_MODE_API_KEY, QUICKSTART_AUTH_MODE_CODEX];
const QUICKSTART_ANTHROPIC_AUTH_MODES: &[&str] = &[
    QUICKSTART_AUTH_MODE_API_KEY,
    QUICKSTART_AUTH_MODE_SETUP_TOKEN,
];

fn auth_modes_for(provider_type: &str) -> Option<&'static [&'static str]> {
    match provider_type {
        "openai" => Some(QUICKSTART_OPENAI_AUTH_MODES),
        "anthropic" => Some(QUICKSTART_ANTHROPIC_AUTH_MODES),
        _ => None,
    }
}

fn auth_mode_descriptor(
    provider_type: &str,
    codex_auth_preselected: bool,
) -> Option<FieldDescriptor> {
    let modes = auth_modes_for(provider_type)?;
    let (label_key, help_key) = match provider_type {
        "openai" => (
            "cli-quickstart-openai-auth-mode-label",
            "cli-quickstart-openai-auth-mode-help",
        ),
        "anthropic" => (
            "cli-quickstart-anthropic-auth-mode-label",
            "cli-quickstart-anthropic-auth-mode-help",
        ),
        _ => return None,
    };
    let default = if provider_type == "openai" && codex_auth_preselected {
        QUICKSTART_AUTH_MODE_CODEX
    } else {
        QUICKSTART_AUTH_MODE_API_KEY
    };
    Some(FieldDescriptor {
        key: QUICKSTART_AUTH_MODE_FIELD.to_string(),
        label: crate::i18n::get_required_cli_string(label_key),
        help: crate::i18n::get_required_cli_string(help_key),
        kind: zeroclaw_api::config_metadata::PropKind::Enum,
        is_secret: false,
        enum_variants: Some(modes.iter().map(|mode| (*mode).to_string()).collect()),
        required: true,
        default: Some(default.to_string()),
    })
}

fn apply_into(
    config: &mut Config,
    submission: &BuilderSubmission,
    staged_files: &mut Vec<StagedPersonalityWrite>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> Option<AppliedAgent> {
    if !validate_runtime_profile_choice(config, &submission.runtime_profile, errors, ctx) {
        return None;
    }
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

    let runtime_alias = apply_named_preset(
        config,
        &submission.runtime_profile,
        QuickstartStep::RuntimeProfile,
        runtime_preset_keys,
        write_runtime_preset,
        errors,
        ctx,
    )?;
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

fn validate_runtime_profile_choice(
    config: &Config,
    choice: &SelectorChoice<String>,
    errors: &mut Vec<QuickstartError>,
    ctx: Option<&RunCtx>,
) -> bool {
    let message = match choice {
        SelectorChoice::Existing(alias) if !config.runtime_profiles.contains_key(alias) => {
            Some(QuickstartError::for_surface(
                ctx,
                QuickstartStep::RuntimeProfile,
                "",
                format!("no `{alias}` profile configured"),
                "cli-quickstart-error-no-profile",
                &[("alias", alias)],
            ))
        }
        SelectorChoice::Fresh(preset_name) if runtime_preset(preset_name).is_none() => {
            Some(QuickstartError::new(
                QuickstartStep::RuntimeProfile,
                "",
                format!("unknown runtime preset `{preset_name}`"),
            ))
        }
        _ => None,
    };
    if let Some(error) = message {
        errors.push(error);
        false
    } else {
        true
    }
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
            // `openai-codex` is accepted as a convenience input alias and
            // normalizes to the existing `openai` config family.
            let Some((provider_type, codex_alias_requested)) =
                resolve_model_provider_type(&choice.provider_type)
            else {
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
            };
            let default_auth_mode = if codex_alias_requested {
                QUICKSTART_AUTH_MODE_CODEX
            } else {
                QUICKSTART_AUTH_MODE_API_KEY
            };
            let auth_mode = choice
                .fields
                .get(QUICKSTART_AUTH_MODE_FIELD)
                .map(|value| value.trim().to_ascii_lowercase())
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default_auth_mode.to_string());
            if let Some(allowed_modes) = auth_modes_for(provider_type)
                && !allowed_modes.contains(&auth_mode.as_str())
            {
                let (provider_name, error_key) = if provider_type == "openai" {
                    ("OpenAI", "cli-quickstart-error-unknown-openai-auth-mode")
                } else {
                    (
                        "Anthropic",
                        "cli-quickstart-error-unknown-anthropic-auth-mode",
                    )
                };
                let expected = allowed_modes
                    .iter()
                    .map(|mode| format!("`{mode}`"))
                    .collect::<Vec<_>>()
                    .join(" or ");
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::ModelProvider,
                    QUICKSTART_AUTH_MODE_FIELD,
                    format!("unknown {provider_name} auth mode `{auth_mode}` — pick {expected}"),
                    error_key,
                    &[("mode", &auth_mode)],
                ));
                return None;
            }
            if section_has_alias(config, "providers.models", provider_type, &choice.alias) {
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
            let codex_auth = provider_type == "openai" && auth_mode == QUICKSTART_AUTH_MODE_CODEX;
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
            if codex_auth {
                if let Err(err) = config
                    .set_prop_persistent(&format!("{prefix}.wire_api"), WireApi::Responses.as_str())
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        "wire_api",
                        err.to_string(),
                    ));
                    return None;
                }
                if let Err(err) =
                    config.set_prop_persistent(&format!("{prefix}.requires_openai_auth"), "true")
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::ModelProvider,
                        "requires_openai_auth",
                        err.to_string(),
                    ));
                    return None;
                }
            }
            // Round-trip every field the surface echoed back. Keys are
            // whatever `field_shape()` emitted — the daemon authored
            // them, so it knows where they go.
            let mut entries: Vec<(&String, &String)> = choice.fields.iter().collect();
            entries.sort_by(|a, b| a.0.cmp(b.0));
            for (key, value) in entries {
                if key == QUICKSTART_AUTH_MODE_FIELD {
                    continue;
                }
                if codex_auth
                    && matches!(
                        key.as_str(),
                        "api_key" | "wire_api" | "requires_openai_auth"
                    )
                {
                    continue;
                }
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
            // Only runs on multi-threaded Tokio runtime (actual CLI), not single-threaded test runtime.
            let provider_config = zeroclaw_config::schema::ModelProviderConfig {
                model: Some(choice.model.clone()),
                uri: config.get_prop(&format!("{prefix}.uri")).ok(),
                api_key: choice
                    .fields
                    .get("api_key")
                    .and_then(|v| if v.is_empty() { None } else { Some(v.clone()) }),
                ..Default::default()
            };
            if tokio::runtime::Handle::try_current()
                .map(|h| {
                    matches!(
                        h.runtime_flavor(),
                        tokio::runtime::RuntimeFlavor::MultiThread
                    )
                })
                .unwrap_or(false)
                && let Some(ctx) = tokio::task::block_in_place(|| {
                    tokio::runtime::Handle::current().block_on(
                        zeroclaw_providers::fetch_context_window(provider_type, &provider_config),
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

fn runtime_preset_keys(config: &Config) -> Vec<String> {
    config.runtime_profiles.keys().cloned().collect()
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
            if !storage_has_ref(config, reference) {
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
            let kind_name = serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase());
            if matches!(kind, MemoryChoice::Tachi) && !zeroclaw_memory::TACHI_COMPILED {
                errors.push(QuickstartError::for_surface(
                    ctx,
                    QuickstartStep::Memory,
                    "backend",
                    "memory backend 'tachi' requires a build with `memory-tachi` enabled"
                        .to_string(),
                    "cli-quickstart-error-tachi-not-compiled",
                    &[],
                ));
                return None;
            }
            if matches!(kind, MemoryChoice::None | MemoryChoice::Tachi) {
                if let Err(err) = config.set_prop_persistent("memory.backend", &kind_name) {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Memory,
                        "backend",
                        err.to_string(),
                    ));
                    return None;
                }
                return Some(kind_name);
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

fn usable_quickstart_value(value: &str) -> Option<&str> {
    let value = value.trim();
    (!zeroclaw_config::traits::is_unset_display_value(value)).then_some(value)
}

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
                let submitted_channel_type = entry.channel_type.trim();
                let alias = entry.alias.trim();
                if submitted_channel_type.is_empty() || alias.is_empty() {
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
                let config_channel_type = submitted_channel_type;
                if zeroclaw_config::schema::is_retired_transport(config_channel_type) {
                    // The picker never offers retired transports, but the
                    // submit path must reject them too: nothing may create
                    // new config sections for transports without a runner
                    // (#584 Slice 3 deletes those schemas).
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].channel_type"),
                        format!(
                            "channel type `{submitted_channel_type}` is retired; it has no \
                             runner and cannot be configured"
                        ),
                        "cli-quickstart-error-channel-retired",
                        &[("channel_type", submitted_channel_type)],
                    ));
                    continue;
                }
                if channel_exists(config, config_channel_type, alias) {
                    let alias_ref = format!("{config_channel_type}.{alias}");
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
                let advertised: std::collections::HashSet<String> =
                    field_shape(FieldSection::Channel, &entry.channel_type)
                        .into_iter()
                        .map(|field| field.key)
                        .collect();
                if let Some(key) = entry
                    .fields
                    .keys()
                    .filter(|key| !advertised.contains(*key))
                    .min()
                {
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{key}"),
                        format!("channel field `{key}` is not available in Quickstart"),
                        "cli-quickstart-error-channel-field-not-advertised",
                        &[("field", key.as_str())],
                    ));
                    continue;
                }
                let mut staged = config.clone();
                if let Err(err) =
                    staged.create_map_key(&format!("channels.{config_channel_type}"), alias)
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].channel_type"),
                        err.to_string(),
                    ));
                    continue;
                }
                let prefix = format!("channels.{config_channel_type}.{alias}");
                let mut fields: Vec<_> = entry
                    .fields
                    .iter()
                    .filter_map(|(key, value)| {
                        usable_quickstart_value(value).map(|value| (key, value))
                    })
                    .collect();
                fields.sort_by_key(|(left, _)| *left);

                let mut failed = false;
                for (key, value) in fields {
                    if let Err(err) = staged.set_prop_persistent(&format!("{prefix}.{key}"), value)
                    {
                        errors.push(QuickstartError::new(
                            QuickstartStep::Channels,
                            format!("channels[{idx}].fields.{key}"),
                            err.to_string(),
                        ));
                        failed = true;
                        break;
                    }
                }
                if !failed
                    && let Err(err) =
                        staged.set_prop_persistent(&format!("{prefix}.enabled"), "true")
                {
                    errors.push(QuickstartError::new(
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.enabled"),
                        err.to_string(),
                    ));
                    failed = true;
                }
                if !failed
                    && config_channel_type == "telegram"
                    && let Some(telegram) = staged.channels.telegram.get(alias)
                    && let Err(err) = telegram.validate_bot_token(alias)
                {
                    let structured =
                        zeroclaw_config::api_error::ConfigApiError::from_validation(err);
                    let terminal = structured
                        .path
                        .as_deref()
                        .and_then(|path| path.rsplit('.').next())
                        .unwrap_or("credential");
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{terminal}"),
                        structured.message,
                        "cli-quickstart-error-channel-token-required",
                        &[],
                    ));
                    failed = true;
                }
                if !failed
                    && config_channel_type == "discord"
                    && let Some(discord) = staged.channels.discord.get(alias)
                    && let Err(err) = discord.validate_bot_token(alias)
                {
                    let structured =
                        zeroclaw_config::api_error::ConfigApiError::from_validation(err);
                    let terminal = structured
                        .path
                        .as_deref()
                        .and_then(|path| path.rsplit('.').next())
                        .unwrap_or("credential");
                    errors.push(QuickstartError::for_surface(
                        ctx,
                        QuickstartStep::Channels,
                        format!("channels[{idx}].fields.{terminal}"),
                        structured.message,
                        "cli-quickstart-error-channel-token-required",
                        &[],
                    ));
                    failed = true;
                }
                if failed {
                    continue;
                }
                *config = staged;
                refs.push(format!("{config_channel_type}.{alias}"));
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
        let Some(channel_ref) = canonical_quickstart_channel_ref(&pg.channel) else {
            errors.push(QuickstartError::for_surface(
                ctx,
                QuickstartStep::Channels,
                format!("peer_groups[{idx}].channel"),
                format!("`{}` is not a `<type>.<alias>` reference", pg.channel),
                "cli-quickstart-error-not-type-alias-ref",
                &[("reference", &pg.channel)],
            ));
            continue;
        };
        // Channel ref must resolve to either a channel already in config
        // OR a channel staged in this same submission.
        let staged_match = staged_channel_refs.iter().any(|r| r == &channel_ref);
        let configured_match = match split_ref(&channel_ref) {
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
        if let Err(err) = config.set_prop_persistent(&format!("{prefix}.channel"), &channel_ref) {
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
    // Operator-facing surface: route through the shared guard so onboarding an
    // agent literally named `default` is refused (the reserved runtime fallback),
    // symmetric with the create/RPC/CLI surfaces. Non-`default` names create as
    // before.
    if let Err(err) =
        zeroclaw_config::alias_refs::create_map_key_checked(config, "agents", &identity.name)
    {
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

fn section_has_alias(config: &Config, prefix: &str, family: &str, alias: &str) -> bool {
    for probe_field in ["enabled", "model", "uri"] {
        let probe = format!("{prefix}.{family}.{alias}.{probe_field}");
        if config.get_prop(&probe).is_ok() {
            return true;
        }
    }
    false
}

fn storage_has_ref(config: &Config, reference: &str) -> bool {
    collect_aliased_refs(&config.storage)
        .iter()
        .any(|configured| configured == reference)
}

pub async fn model_catalog(
    model_provider: &str,
) -> (
    Vec<String>,
    Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
    bool,
) {
    model_catalog_with_config(None, model_provider).await
}

pub async fn model_catalog_with_config(
    config: Option<&Config>,
    model_provider: &str,
) -> (
    Vec<String>,
    Option<std::collections::HashMap<String, zeroclaw_api::model_provider::ModelPricing>>,
    bool,
) {
    let resolved = config.and_then(|cfg| cfg.providers.models.find_by_name(model_provider));
    let api_key = resolved
        .as_ref()
        .and_then(|(_family, _alias, base)| base.api_key.clone());
    // Honor a configured custom endpoint so proxied / self-hosted OpenAI-compatible
    // deployments list from their own `/models` rather than the family default.
    let api_url = resolved.as_ref().and_then(|(_family, _alias, base)| {
        base.uri
            .as_deref()
            .map(str::trim)
            .filter(|u| !u.is_empty())
            .map(ToString::to_string)
    });
    // `create_model_provider` and the chat-catalog ranker expect a bare family
    // name, not a dotted ref. Prefer the family `find_by_name` resolved; when it
    // could not (no config / unknown alias) strip any `<family>.<alias>` suffix
    // ourselves so a dotted selector still constructs.
    let family: &str = resolved
        .as_ref()
        .map(|(family, _alias, _base)| *family)
        .unwrap_or_else(|| {
            model_provider
                .split_once('.')
                .map_or(model_provider, |(f, _)| f)
        });

    let handle = zeroclaw_providers::create_model_provider_with_url(
        family,
        api_key.as_deref(),
        api_url.as_deref(),
    );
    if let Ok(handle) = handle
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
        let Some(ids) = zeroclaw_providers::catalog::sort_model_catalog_for_chat(family, ids)
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
    match zeroclaw_providers::catalog::list_models_for_family(family).await {
        Ok(models) if !models.is_empty() => (
            zeroclaw_providers::catalog::sort_model_catalog_for_chat(family, models)
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
mod tests;
