//! Onboard catalog endpoint — exposes the model_provider + model catalog the CLI
//! wizard already uses, so the dashboard's "+ Add model_provider" affordance and
//! model-picker dropdown share the same source of truth as the CLI.
//!
//! No catalog data is hand-maintained at this layer. `list_model_providers()` lives
//! in `zeroclaw-providers` and is the canonical list; `list_models()` per
//! model_provider fetches from models.dev (cached) or the model_provider's own /models
//! endpoint. Same code paths as the CLI wizard.
//!
//!

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};

use super::AppState;
use super::api::require_auth;

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CatalogModelProvider {
    /// Canonical model_provider name as used in `[model_providers.<name>]`.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether the model model_provider is fully local (no API key required).
    pub local: bool,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CatalogResponse {
    pub model_providers: Vec<CatalogModelProvider>,
}

/// `GET /api/onboard/catalog` — list every model provider the CLI wizard knows
/// about. The dashboard shows these in the "+ Add model provider" picker so
/// CLI / web stay in sync.
pub async fn handle_catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state;

    let model_providers: Vec<CatalogModelProvider> = zeroclaw_providers::list_model_providers()
        .into_iter()
        .map(|p| CatalogModelProvider {
            name: p.name.to_string(),
            display_name: p.display_name.to_string(),
            local: p.local,
        })
        .collect();

    axum::Json(CatalogResponse { model_providers }).into_response()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsQuery {
    /// ModelProvider name (canonical, from CatalogModelProvider.name).
    /// `provider` alias matches the query-string name the web dashboard uses.
    #[serde(alias = "provider")]
    pub model_provider: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsResponse {
    pub model_provider: String,
    pub models: Vec<String>,
    /// Whether this provider family is local according to the canonical
    /// provider catalog.
    pub local: bool,
    /// `true` when the catalog was fetched live; `false` if the cache was
    /// served (or if this model_provider has no remote catalog and the empty list
    /// is the genuine answer).
    pub live: bool,
}

/// `GET /api/onboard/catalog/models?model_provider=<name>` — fetch the model list
/// for one model_provider. Same code path the CLI wizard uses
/// (`zeroclaw_providers::create_model_provider(...).list_models()`), which goes
/// through the models.dev cached catalog for OpenAI / Anthropic / Gemini,
/// the live `/v1/models` endpoint for OpenRouter, etc.
///
/// Lazy: the dashboard hits this only when the user picks a model_provider, so
/// initial catalog load stays fast. Fetch failures return an empty list
/// with `live: false` so the form falls back to a free-text input.
pub async fn handle_catalog_models(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<ModelsQuery>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state;
    let local = model_provider_family_is_local(&q.model_provider);

    // Try provider construction + list_models first (covers credentialed
    // /models endpoints). Fall back to the family catalog table when
    // construction or list_models fails — this is the path that covers
    // onboard mode where the operator hasn't supplied a credential
    // and the provider has typed required fields (Azure resource,
    // Bedrock region, …) that haven't been populated yet.
    let provider_path = match zeroclaw_providers::create_model_provider(&q.model_provider, None) {
        Ok(h) => match h.list_models().await {
            Ok(ms) if !ms.is_empty() => Some(ms),
            _ => None,
        },
        Err(_) => None,
    };

    let (models, live) = match provider_path {
        Some(ms) => (ms, true),
        None => {
            match zeroclaw_providers::catalog::list_models_for_family(&q.model_provider).await {
                Ok(ms) => (ms, true),
                Err(e) => {
                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": q.model_provider, "error": format!("{}", e)})), "model catalog fetch failed");
                    (Vec::new(), false)
                }
            }
        }
    };

    axum::Json(ModelsResponse {
        model_provider: q.model_provider,
        models,
        local,
        live,
    })
    .into_response()
}

fn error_response(err: ConfigApiError) -> Response {
    let status = axum::http::StatusCode::from_u16(err.code.http_status())
        .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR);
    (status, axum::Json(err)).into_response()
}

// ── Section + picker (mirrors the TUI flow) ──────────────────────────

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionInfo {
    /// Stable section key — `model_providers`, `channels`, `memory`,
    /// `hardware`, `tunnel`. Matches `Section::as_path_prefix` in
    /// zeroclaw-runtime so CLI / web stay aligned.
    pub key: String,
    /// Human-readable section name for headers / breadcrumbs.
    pub label: String,
    /// Help text the wizard shows under the section title.
    pub help: String,
    /// `true` when this section requires picking an item before the form
    /// renders (Providers / Channels / Memory / Tunnel). `false` for sections
    /// that have a single direct form (Hardware).
    pub has_picker: bool,
    /// Whether the user has marked the section completed in
    /// `onboard_state.completed_sections`.
    pub completed: bool,
    /// Whether the section currently has enough usable config for the
    /// first-run path. This is stricter than `completed`: visiting a section
    /// can mark it completed, but the sidebar checkmark should not imply a
    /// provider or agent is runnable when required fields are still missing.
    pub ready: bool,
    /// Display group for the dashboard sidebar (`Foundation`, `Agent`,
    /// `Tools`, etc.). Curated server-side until v3 / #5947 lands a schema
    /// attribute that encodes the grouping declaratively.
    pub group: String,
    /// `true` when this section is part of `/onboard`'s canonical
    /// section list (`zeroclaw_config::sections::ONBOARDING_SECTIONS`).
    /// Since the wizard/explorer split was retired, every known section
    /// returns `true`; the field is preserved for API stability so the
    /// frontend's `.filter((s) => s.is_onboarding)` stays a no-op rather
    /// than failing to compile.
    pub is_onboarding: bool,
    /// Editor shape (direct form / one-tier alias map / typed-family map /
    /// backend picker). Server-emitted from
    /// `zeroclaw_config::sections::Section::shape()`; both the
    /// dashboard explorer and the onboard wizard dispatch their renderer
    /// off this flag so identical sections render identically.
    /// `None` for sections that aren't part of the canonical wizard.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shape: Option<zeroclaw_config::sections::SectionShape>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionsResponse {
    pub sections: Vec<SectionInfo>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OnboardStatusResponse {
    /// `true` when no agent is dispatchable yet. The dashboard uses this
    /// signal to redirect first-load visits from `/` to `/onboard`.
    pub needs_onboarding: bool,
    /// Short machine-readable reason for the value of `needs_onboarding`,
    /// for logs / debugging. Stable: `fresh_install` / `incomplete_agent`
    /// / `has_dispatchable_agent`.
    pub reason: &'static str,
    /// `true` when the operator has started entering setup state even if no
    /// agent can reply yet. The dashboard uses this to say "Continue
    /// onboarding" instead of pretending the flow is fresh.
    pub has_partial_state: bool,
    /// Human-readable readiness failures. When onboarding cannot finish, the
    /// UI shows these directly so the operator knows exactly what is missing.
    pub missing: Vec<String>,
}

/// Pure derivation of the onboard-status response from a config snapshot.
/// `needs_onboarding` is `false` iff at least one enabled `[agents.<alias>]`
/// block has a resolved model provider with a selected model plus resolved
/// risk/runtime profile refs. A provider without a bound, runnable agent is
/// not a completion signal: chat dispatch still bounces with a setup error in
/// that state.
#[must_use]
pub fn derive_onboard_status(cfg: &zeroclaw_config::schema::Config) -> OnboardStatusResponse {
    let missing = onboard_missing_requirements(cfg);
    let ready = missing.is_empty();
    let has_partial_state = !cfg.onboard_state.completed_sections.is_empty()
        || cfg.providers.models.iter_entries().next().is_some()
        || !cfg.risk_profiles.is_empty()
        || !cfg.runtime_profiles.is_empty()
        || !cfg.agents.is_empty();
    let reason = if ready {
        "has_dispatchable_agent"
    } else if has_partial_state {
        "incomplete_agent"
    } else {
        "fresh_install"
    };
    OnboardStatusResponse {
        needs_onboarding: !ready,
        reason,
        has_partial_state,
        missing,
    }
}

fn onboard_missing_requirements(cfg: &zeroclaw_config::schema::Config) -> Vec<String> {
    let mut missing = Vec::new();
    if cfg.providers.models.iter_entries().next().is_none() {
        missing.push("Add a model provider.".to_string());
    }
    if cfg.agents.is_empty() {
        missing.push("Create an agent.".to_string());
        return missing;
    }

    let mut agent_aliases: Vec<&String> = cfg.agents.keys().collect();
    agent_aliases.sort();
    let mut has_dispatchable_agent = false;
    for alias in agent_aliases {
        let agent_missing = onboard_agent_missing_requirements(cfg, alias, &cfg.agents[alias]);
        if agent_missing.is_empty() {
            has_dispatchable_agent = true;
            break;
        }
        missing.extend(agent_missing);
    }
    if has_dispatchable_agent {
        missing.clear();
    }
    missing
}

fn onboard_agent_missing_requirements(
    cfg: &zeroclaw_config::schema::Config,
    alias: &str,
    agent: &zeroclaw_config::schema::AliasedAgentConfig,
) -> Vec<String> {
    let mut missing = Vec::new();
    if !agent.enabled {
        missing.push(format!("Enable agent `{alias}`."));
    }

    let model_ref = agent.model_provider.trim();
    if model_ref.is_empty() {
        missing.push(format!("Set a model provider for agent `{alias}`."));
    } else if let Some((family, _, provider)) = cfg.resolved_model_provider_for_agent(alias) {
        let has_model = provider
            .model
            .as_deref()
            .map(str::trim)
            .is_some_and(|m| !m.is_empty());
        if !has_model {
            missing.push(format!("Choose a model for model provider `{model_ref}`."));
        } else if !model_provider_alias_usable(provider, model_provider_family_is_local(family)) {
            missing.push(format!(
                "Set credential/auth for model provider `{model_ref}`."
            ));
        }
    } else {
        missing.push(format!(
            "Fix agent `{alias}` model provider `{model_ref}`; it does not resolve to a configured provider."
        ));
    }

    let risk_ref = agent.risk_profile.trim();
    if risk_ref.is_empty() {
        missing.push(format!("Set a risk profile for agent `{alias}`."));
    } else if !cfg.risk_profiles.contains_key(risk_ref) {
        missing.push(format!(
            "Fix agent `{alias}` risk profile `{risk_ref}`; it does not resolve to a configured profile."
        ));
    }

    let runtime_ref = agent.runtime_profile.trim();
    if runtime_ref.is_empty() {
        missing.push(format!("Set a runtime profile for agent `{alias}`."));
    } else if !cfg.runtime_profiles.contains_key(runtime_ref) {
        missing.push(format!(
            "Fix agent `{alias}` runtime profile `{runtime_ref}`; it does not resolve to a configured profile."
        ));
    }

    missing
}

/// `GET /api/onboard/status` — boolean signal for the dashboard's
/// fresh-install redirect. The daemon writes a default `config.toml` on
/// first init, so file existence isn't a useful "is the user new?" check.
/// Onboarding is complete iff at least one agent has its
/// `model_provider`, `risk_profile`, and `runtime_profile` bound.
pub async fn handle_onboard_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    axum::Json(derive_onboard_status(&cfg)).into_response()
}

/// All alias-reference choices an agent form needs, in one round-trip.
/// Channels and model model_providers are returned in dotted form
/// (`telegram.default`, `anthropic.work`); the bundle/profile/namespace
/// lists are bare HashMap keys.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct AgentOptionsResponse {
    pub channels: Vec<String>,
    /// Distinct channel types with at least one configured alias —
    /// `["discord", "telegram"]`. Source for peer-group channel picker.
    pub channel_types: Vec<String>,
    pub model_providers: Vec<String>,
    pub risk_profiles: Vec<String>,
    pub runtime_profiles: Vec<String>,
    pub skill_bundles: Vec<String>,
    pub knowledge_bundles: Vec<String>,
    pub mcp_bundles: Vec<String>,
    pub agents: Vec<String>,
}

/// Build the `AgentOptionsResponse` from a config snapshot. Pure function
/// so tests can drive the same code path the handler runs without spinning
/// up an `AppState`.
///
/// `get_map_keys` expects **kebab-case** paths (the macro at
/// `crates/zeroclaw-macros/src/lib.rs:366` builds lookup arms with
/// `snake_to_kebab(field_name)`). Passing snake_case for any
/// underscore-bearing field silently returns `None` → empty `Vec` →
/// dashboard renders "No X configured yet" even though X is configured.
pub fn build_agent_options(cfg: &zeroclaw_config::schema::Config) -> AgentOptionsResponse {
    fn dotted_aliases(cfg: &zeroclaw_config::schema::Config, prefix: &str) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for f in cfg.prop_fields() {
            if let Some(rest) = f.name.strip_prefix(&format!("{prefix}.")) {
                let mut parts = rest.splitn(3, '.');
                if let (Some(ty), Some(alias), Some(_)) = (parts.next(), parts.next(), parts.next())
                {
                    let dotted = format!("{ty}.{alias}");
                    if !out.contains(&dotted) {
                        out.push(dotted);
                    }
                }
            }
        }
        out.sort();
        out
    }

    let channels = dotted_aliases(cfg, "channels");
    let mut channel_types: Vec<String> = channels
        .iter()
        .filter_map(|d| d.split_once('.').map(|(t, _)| t.to_string()))
        .collect();
    channel_types.sort();
    channel_types.dedup();

    AgentOptionsResponse {
        channels,
        channel_types,
        model_providers: dotted_aliases(cfg, "providers.models"),
        risk_profiles: cfg.get_map_keys("risk-profiles").unwrap_or_default(),
        runtime_profiles: cfg.get_map_keys("runtime-profiles").unwrap_or_default(),
        skill_bundles: cfg.get_map_keys("skill-bundles").unwrap_or_default(),
        knowledge_bundles: cfg.get_map_keys("knowledge-bundles").unwrap_or_default(),
        mcp_bundles: cfg.get_map_keys("mcp-bundles").unwrap_or_default(),
        agents: cfg.get_map_keys("agents").unwrap_or_default(),
    }
}

/// `GET /api/onboard/agent-options` — every alias-reference list the
/// agent form needs, derived from the live config. Mirrors the lists the
/// TUI computes locally for its alias pickers.
pub async fn handle_agent_options(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    axum::Json(build_agent_options(&cfg)).into_response()
}

/// `GET /api/onboard/sections` — list every top-level config section.
///
/// Schema-driven: walks `Config::prop_fields()` and collects unique first
/// segments, then asks `Config::map_key_sections()` for which ones have
/// pickers. The 4 onboarding sections (`model_providers`, `channels`, `memory`,
/// `tunnel`) keep their existing per-section dispatch in
/// `handle_section_picker`; everything else (`gateway`, `observability`,
/// `scheduler`, ...) renders as a direct form. Adding a new top-level
/// field to `Config` makes it appear here automatically.
pub async fn handle_sections(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();
    let completed: std::collections::HashSet<String> = cfg
        .onboard_state
        .completed_sections
        .iter()
        .cloned()
        .collect();

    // First segment of every reachable prop path. BTreeSet for stable
    // alphabetical order and dedup.
    let mut roots: std::collections::BTreeSet<String> = cfg
        .prop_fields()
        .iter()
        .filter_map(|f| f.name.split('.').next().map(str::to_string))
        .collect();

    // System / housekeeping fields the user never edits via the dashboard.
    for hidden in HIDDEN_TOP_LEVEL {
        roots.remove(*hidden);
    }

    // A section gets a picker only when its OWN root carries a map (path
    // == key) or its immediate child is a typed-family map (path == key
    // + "." + one segment). Deeper nested maps belong to a subsection's
    // own editor and must not promote their top-level section to a
    // picker — `cost.rates.providers.models.<type>` is the rate-sheet's
    // concern, not a reason to give `[cost]` an Add affordance.
    let all_map_paths: Vec<&'static str> = zeroclaw_config::schema::Config::map_key_sections()
        .iter()
        .map(|s| s.path)
        .collect();
    let section_has_picker_for_key = |key: &str| -> bool {
        let key_dot = format!("{key}.");
        all_map_paths.iter().any(|p| {
            *p == key
                || p.strip_prefix(&key_dot)
                    .is_some_and(|rest| !rest.contains('.'))
        })
    };

    // Ensure map-keyed sections surface as sidebar entries even when their
    // HashMap is empty (prop_fields() only yields paths for populated
    // entries). First segments only — the prefix-dedup pass below drops
    // bare parent segments when a multi-segment child is present.
    let map_keyed_roots: std::collections::HashSet<&'static str> = all_map_paths
        .iter()
        .filter_map(|p| p.split('.').next())
        .collect();
    for &prefix in &map_keyed_roots {
        roots.insert(prefix.to_string());
    }

    // Synthetic onboarding sections — keys that aren't fields on Config
    // but are part of the wizard flow (personality lives as markdown
    // files, not TOML). Inject so the canonical-order sort places them
    // correctly and frontends don't need to know which ones to splice.
    for s in zeroclaw_config::sections::ONBOARDING_SECTIONS {
        roots.insert(s.as_str().to_string());
    }

    // Drop bare parent-segment entries when a dotted child is present
    // — `providers` is phantom once `providers.models` etc. are listed.
    let prefixes_with_children: std::collections::HashSet<String> = roots
        .iter()
        .filter_map(|k| k.split_once('.').map(|(parent, _)| parent.to_string()))
        .collect();
    roots.retain(|k| k.contains('.') || !prefixes_with_children.contains(k));

    // Hard-ban the rate-sheet subtree from the sidebar. `[cost.rates.*]` is
    // edited from inside the `[cost]` section's tabs (and from each
    // provider-type page's Costs tab); it has no standalone picker, no
    // direct form at any intermediate depth, and surfacing a path like
    // `cost.rates.providers.tts` as its own sidebar entry only yields a
    // dead-end "no picker" page.
    roots.retain(|k| !k.starts_with("cost.rates"));

    // Sort: onboarding-wizard sections first in their canonical order
    // (single source of truth in `zeroclaw_config::sections`), then
    // everything else alphabetically. This is what makes /onboard's wizard
    // order and /config's foundation grouping derive from one Rust const
    // — frontends consume the response order directly.
    let mut ordered: Vec<String> = roots.into_iter().collect();
    ordered.sort_by(|a, b| {
        match (
            zeroclaw_config::sections::section_index_for_key(a),
            zeroclaw_config::sections::section_index_for_key(b),
        ) {
            (Some(ai), Some(bi)) => ai.cmp(&bi),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => a.cmp(b),
        }
    });

    let sections: Vec<SectionInfo> = ordered
        .into_iter()
        .map(|key| {
            // Picker eligibility = anything `handle_section_picker`
            // dispatches non-trivially. Wizard sections that opt out
            // (workspace/hardware/personality) are direct-form. Map-keyed
            // sections outside the wizard (multi-agent peer groups, etc.)
            // get the generic schema-walk picker.
            let wizard = zeroclaw_config::sections::Section::from_key(&key);
            let has_picker = match wizard {
                Some(w) => !matches!(
                    w,
                    zeroclaw_config::sections::Section::Hardware
                        | zeroclaw_config::sections::Section::Mcp
                        | zeroclaw_config::sections::Section::Skills
                ),
                None => section_has_picker_for_key(&key),
            };
            SectionInfo {
                completed: completed.contains(&key),
                ready: section_ready(&cfg, &key, completed.contains(&key)),
                label: humanize_section(&key),
                help: section_help(&key).to_string(),
                has_picker,
                group: section_group(&key).to_string(),
                is_onboarding: wizard.is_some(),
                shape: wizard.map(zeroclaw_config::sections::Section::shape),
                key,
            }
        })
        .collect();

    axum::Json(SectionsResponse { sections }).into_response()
}

fn section_ready(cfg: &zeroclaw_config::schema::Config, key: &str, completed_marker: bool) -> bool {
    use zeroclaw_config::sections::Section;
    match Section::from_key(key) {
        Some(Section::ModelProviders) => any_usable_model_provider(cfg),
        Some(Section::RiskProfiles) => !cfg.risk_profiles.is_empty(),
        Some(Section::RuntimeProfiles) => !cfg.runtime_profiles.is_empty(),
        Some(Section::Storage) => cfg
            .prop_fields()
            .iter()
            .any(|field| field.name.starts_with("storage.")),
        Some(Section::Memory) => completed_marker,
        Some(Section::Agents) => cfg
            .agents
            .iter()
            .any(|(alias, agent)| onboard_agent_missing_requirements(cfg, alias, agent).is_empty()),
        _ => completed_marker,
    }
}

/// Top-level fields that exist on `Config` but are never user-editable
/// from the dashboard (schema bookkeeping, resolved at runtime).
const HIDDEN_TOP_LEVEL: &[&str] = &[
    "schema_version",
    "onboard_state",
    "onboard-state",
    "config_path",
    "workspace_dir",
    "env_overridden_paths",
    "pre_override_snapshots",
];

/// Humanize a section key for display (`google_workspace` → `Google workspace`).
/// Keeps things simple and predictable; specific wording overrides go in
/// the section-help table or per-section labels if/when we add them.
fn humanize_section(key: &str) -> String {
    match key {
        "providers.models" => return "Model providers".to_string(),
        "providers.tts" => return "TTS providers".to_string(),
        "providers.transcription" => return "Transcription providers".to_string(),
        _ => {}
    }
    let mut s = key.replace(['_', '-'], " ");
    if let Some(c) = s.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    s
}

/// Display group for a section. Hand-curated until v3 / #5947 lands a
/// schema attribute that encodes grouping declaratively. Unknown keys
/// fall into `Other` so new schema additions still surface — they just
/// land in the catch-all bucket until someone curates them.
///
/// Group order in the dashboard sidebar is governed by the frontend (see
/// `Config.tsx`), not this list.
fn section_group(key: &str) -> &'static str {
    match key {
        "providers.models" | "channels" | "memory" | "hardware" | "tunnel" | "agents"
        | "skills" | "skill-bundles" | "risk-profiles" | "runtime-profiles" | "peer-groups" => {
            "Foundation"
        }
        // Agent loop, scheduling, and orchestration.
        "agent"
        | "cron"
        | "heartbeat"
        | "hooks"
        | "pacing"
        | "pipeline"
        | "query_classification"
        | "reliability"
        | "runtime"
        | "scheduler"
        | "sop"
        | "verifiable_intent" => "Agent",
        // Multi-agent / delegation.
        "delegate" => "Multi-agent",
        // Tool integrations.
        "browser" | "browser_delegate" | "http_request" | "image_gen" | "knowledge"
        | "link_enricher" | "mcp" | "media_pipeline" | "multimodal" | "plugins"
        | "project_intel" | "shell_tool" | "text_browser" | "transcription" | "tts"
        | "web_fetch" | "web_search" => "Tools",
        // External services / vendor integrations. ACP is included because
        // it is always client-paired — you cannot use it without a client.
        "acp" | "claude_code" | "claude_code_runner" | "codex_cli" | "composio" | "gemini_cli"
        | "google_workspace" | "jira" | "linkedin" | "notion" | "opencode_cli" => "Integrations",
        // Networking / multi-node infrastructure.
        "gateway" | "node_transport" | "nodes" | "proxy" => "Network",
        // Storage, identity, secrets.
        "identity" | "secrets" | "storage" => "Storage",
        // Operations / monitoring / safety / cost.
        "backup" | "cloud_ops" | "conversational_ai" | "cost" | "data_retention"
        | "observability" | "peripherals" | "security" | "security_ops" | "trust" => "Operations",
        _ => "Other",
    }
}

/// Help text for a section. Delegates to `zeroclaw_config::sections::section_help`
/// so gateway, CLI, and TUI all read from one source — wizard variants
/// pull from `Section::help`, everything else from the matching
/// `#[nested]` field's `///` docstring on the `Config` struct.
fn section_help(key: &str) -> &'static str {
    zeroclaw_config::sections::section_help(key)
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionPath {
    pub section: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PickerItem {
    /// Stable identifier — what the frontend POSTs back to select this item.
    pub key: String,
    /// Human-readable label for display (catalog display_name, channel name,
    /// memory backend label, etc.).
    pub label: String,
    /// Optional secondary line under the label (e.g. memory backend's
    /// extended description, "(local)" for local-only model_providers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional badge — `"set"` / `"needs setup"` / `"created"` /
    /// `"configured"` / `"active"` depending on section semantics. The
    /// frontend uses this to mark rows distinct without overstating readiness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct PickerResponse {
    pub section: String,
    pub items: Vec<PickerItem>,
    /// Help text for the picker (re-served from the section info so the
    /// frontend doesn't need to round-trip).
    pub help: String,
}

/// `GET /api/onboard/sections/<section>` — picker items for that section.
///
/// Per-section dispatch:
/// * `providers` → `zeroclaw_providers::list_model_providers()` (CLI's catalog).
/// * `memory` → `zeroclaw_memory::selectable_memory_backends()`.
/// * `channels` / `tunnel` → schema-walk: clone config, `init_defaults` the
///   section, then strip the section prefix from `prop_fields()` and dedupe
///   by first segment. Same trick the TUI uses; new channels appear
///   automatically when a `#[nested] Option<...>` field is added.
/// * Anything else returns 404 (hardware has no picker).
pub async fn handle_section_picker(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(SectionPath { section }): axum::extract::Path<SectionPath>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.read().clone();

    use zeroclaw_config::sections::Section;
    let Some(section_enum) = Section::from_key(&section) else {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!(
                    "section `{section}` has no picker; render its fields \
                     via GET /api/config/list?prefix={section}"
                ),
            )
            .with_path(section.as_str()),
        );
    };
    let help = section_help(section_enum.as_str()).to_string();
    let items = match picker_items_for(section_enum, &cfg) {
        PickerDispatch::Items(items) => items,
        PickerDispatch::DirectForm => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!(
                        "section `{section_enum}` is a direct-form section with no picker; \
                         render fields via GET /api/config/list?prefix={section_enum}"
                    ),
                )
                .with_path(section_enum.as_str()),
            );
        }
    };

    axum::Json(PickerResponse {
        section,
        items,
        help,
    })
    .into_response()
}

/// Result of picker dispatch for a [`Section`]. `Items` carries the
/// list rendered into the dashboard / CLI picker UI; `DirectForm`
/// signals a section without a picker step (the caller falls through
/// to `/api/config/list?prefix=<section>` for direct field rendering).
///
/// Splitting this out from `handle_section_picker` keeps the per-Section
/// dispatch a pure function — testable without an `AppState` mock and
/// exhaustively coverable by iterating every variant.
enum PickerDispatch {
    Items(Vec<PickerItem>),
    DirectForm,
}

/// Per-section picker dispatch. Exhaustive over [`Section`] so adding a
/// variant fails to compile until it gets a routing arm. The DRY
/// version of what the dashboard's per-section view boils down to.
fn picker_items_for(
    section: zeroclaw_config::sections::Section,
    cfg: &zeroclaw_config::schema::Config,
) -> PickerDispatch {
    use zeroclaw_config::sections::Section;
    match section {
        Section::ModelProviders => PickerDispatch::Items(providers_picker(cfg)),
        // TTS / transcription share the typed-family two-tier shape. Each
        // family enumerates its picker via `schema_walk_picker(<family>)`
        // — the same machinery channels uses, so no per-section catalog
        // table to drift.
        Section::TtsProviders | Section::TranscriptionProviders => {
            PickerDispatch::Items(schema_walk_picker(cfg, section.as_str()))
        }
        Section::Memory => PickerDispatch::Items(memory_picker(cfg)),
        Section::Channels => PickerDispatch::Items(schema_walk_picker(cfg, "channels")),
        Section::Tunnel => PickerDispatch::Items(schema_walk_picker_with_none(
            cfg,
            "tunnel",
            "tunnel.tunnel-provider",
        )),
        Section::Agents => PickerDispatch::Items(agents_picker(cfg)),
        // Storage is two-tier (`storage.<kind>.<alias>`) — same shape
        // and walker as channels and the typed-provider families.
        Section::Storage => PickerDispatch::Items(storage_picker(cfg)),
        // OneTierAliasMap explorer sections: pick a key from the live
        // HashMap. Generic walker covers every section whose schema is
        // `<section>.<alias>` (operator-named keys, no closed kind set).
        Section::PeerGroups
        | Section::Cron
        | Section::McpBundles
        | Section::KnowledgeBundles
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles => {
            PickerDispatch::Items(one_tier_alias_map_picker(cfg, section.as_str()))
        }
        Section::Hardware | Section::Mcp | Section::Skills => PickerDispatch::DirectForm,
    }
}

fn providers_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    zeroclaw_providers::list_model_providers()
        .into_iter()
        .map(|p| PickerItem {
            key: p.name.to_string(),
            label: p.display_name.to_string(),
            description: if p.local {
                Some("Local — no API key required".to_string())
            } else {
                None
            },
            badge: provider_type_badge(cfg, p.name, p.local),
        })
        .collect()
}

fn any_usable_model_provider(cfg: &zeroclaw_config::schema::Config) -> bool {
    cfg.providers
        .models
        .iter_entries()
        .any(|(family, _, base)| {
            model_provider_alias_usable(base, model_provider_family_is_local(family))
        })
}

fn model_provider_family_is_local(family: &str) -> bool {
    zeroclaw_providers::list_model_providers()
        .iter()
        .find(|provider| provider.name == family)
        .is_some_and(|provider| provider.local)
}

fn provider_type_badge(
    cfg: &zeroclaw_config::schema::Config,
    family: &str,
    local: bool,
) -> Option<String> {
    let mut has_alias = false;
    let mut has_usable_alias = false;
    for (ty, _, base) in cfg.providers.models.iter_entries() {
        if ty != family {
            continue;
        }
        has_alias = true;
        if model_provider_alias_usable(base, local) {
            has_usable_alias = true;
        }
    }
    if has_usable_alias {
        Some("configured".to_string())
    } else if has_alias {
        Some("needs setup".to_string())
    } else {
        None
    }
}

fn model_provider_alias_usable(
    base: &zeroclaw_config::schema::ModelProviderConfig,
    local: bool,
) -> bool {
    let has_model = base
        .model
        .as_deref()
        .map(str::trim)
        .is_some_and(|model| !model.is_empty());
    if !has_model {
        return false;
    }
    base.api_key
        .as_deref()
        .map(str::trim)
        .is_some_and(|key| !key.is_empty())
        || base.requires_openai_auth
        || local
}

fn storage_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    let mut items = schema_walk_picker(cfg, "storage");
    for item in &mut items {
        item.description = storage_description(&item.key).map(str::to_string);
        if item.badge.as_deref() == Some("configured") {
            item.badge = Some("created".to_string());
        }
    }
    items.sort_by_key(|item| storage_rank(&item.key));
    items
}

fn storage_rank(key: &str) -> usize {
    match key {
        "sqlite" => 0,
        "postgres" => 1,
        "qdrant" => 2,
        "markdown" => 3,
        "lucid" => 4,
        _ => 99,
    }
}

fn storage_description(key: &str) -> Option<&'static str> {
    match key {
        "sqlite" => Some(
            "Safe default for single-node installs: file-based, zero-config, no external service.",
        ),
        "postgres" => {
            Some("Shared or multi-instance deployments that need durable server-backed storage.")
        }
        "qdrant" => {
            Some("Vector database backend for semantic search when you already run Qdrant.")
        }
        "markdown" => {
            Some("Human-readable files with simple local storage and no database service.")
        }
        "lucid" => {
            Some("Bridge to local lucid-memory CLI while keeping SQLite-style local operation.")
        }
        _ => None,
    }
}

fn memory_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    let current = cfg.memory.backend.clone();
    let memory_completed = cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|section| section == "memory");
    zeroclaw_memory::selectable_memory_backends()
        .iter()
        .map(|b| PickerItem {
            key: b.key.to_string(),
            label: b.label.to_string(),
            description: None,
            badge: if b.key == current && memory_completed {
                Some("active".to_string())
            } else {
                None
            },
        })
        .collect()
}

/// Generic schema-walk picker for sections like `channels` whose subsections
/// are `#[nested] HashMap<String, T>` fields. Discovery: use `map_key_sections()`
/// to enumerate all statically-known sub-sections under `<section>.` — this
/// works for HashMap-based channels without needing init_defaults to insert
/// entries (HashMap fields start empty and init_defaults leaves them empty).
fn schema_walk_picker(cfg: &zeroclaw_config::schema::Config, section: &str) -> Vec<PickerItem> {
    let prefix_with_dot = format!("{section}.");

    // Configured: any alias present on this type (has at least one entry in its HashMap).
    let configured: std::collections::BTreeSet<String> = cfg
        .prop_fields()
        .iter()
        .filter_map(|f| f.name.strip_prefix(&prefix_with_dot))
        .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
        .collect();

    // All known channel/section types from schema metadata — statically known,
    // no HashMap entries needed.
    let all: std::collections::BTreeSet<String> =
        zeroclaw_config::schema::Config::map_key_sections()
            .into_iter()
            .filter_map(|s| {
                s.path
                    .strip_prefix(&prefix_with_dot)
                    .filter(|rest| !rest.contains('.'))
                    .map(String::from)
            })
            .collect();

    all.into_iter()
        .map(|name| {
            // Channel configs no longer carry an `enabled` field; a channel is
            // active when an enabled agent references it. Badge = "configured" when
            // at least one alias exists, absent otherwise.
            let badge = if configured.contains(&name) {
                Some("configured".to_string())
            } else {
                None
            };
            PickerItem {
                key: name.clone(),
                label: name.clone(),
                description: None,
                badge,
            }
        })
        .collect()
}

/// Generic picker for `OneTierAliasMap` sections — walks the live
/// `prop_fields()` for the section prefix and returns one PickerItem
/// per operator-defined alias. The closed-kind enumeration that
/// [`schema_walk_picker`] does via `Config::map_key_sections()` doesn't
/// apply here: aliases under `peer_groups`, `cron`, `risk_profiles`,
/// etc. are operator-named, with no statically-known catalog. Every
/// existing alias is reported `configured`; the dashboard's `+ Add`
/// affordance handles new-key creation through
/// [`handle_select_item`].
fn one_tier_alias_map_picker(
    cfg: &zeroclaw_config::schema::Config,
    section: &str,
) -> Vec<PickerItem> {
    let prefix_with_dot = format!("{section}.");
    let mut keys: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    for field in cfg.prop_fields() {
        let Some(suffix) = field.name.strip_prefix(&prefix_with_dot) else {
            continue;
        };
        let head = suffix.split_once('.').map_or(suffix, |(h, _)| h);
        if head.is_empty() {
            continue;
        }
        keys.insert(head.to_string());
    }
    keys.into_iter()
        .map(|key| PickerItem {
            key: key.clone(),
            label: key,
            description: None,
            badge: Some("configured".to_string()),
        })
        .collect()
}

/// Agents picker: walks `cfg.agents` and returns each alias with an activity badge.
/// `active` = agent exists and `enabled = true`; `configured` = exists but disabled.
fn agents_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    let mut items: Vec<PickerItem> = cfg
        .agents
        .iter()
        .map(|(alias, agent)| PickerItem {
            key: alias.clone(),
            label: alias.clone(),
            description: None,
            badge: if agent.enabled {
                Some("active".to_string())
            } else {
                Some("configured".to_string())
            },
        })
        .collect();
    items.sort_by(|a, b| a.key.cmp(&b.key));
    items
}

fn apply_first_run_agent_defaults(cfg: &mut zeroclaw_config::schema::Config, alias: &str) {
    let model_provider = cfg.first_model_provider_alias();
    let risk_profile = first_alias_preferring_default(cfg.risk_profiles.keys());
    let runtime_profile = first_alias_preferring_default(cfg.runtime_profiles.keys());

    let Some(agent) = cfg.agents.get_mut(alias) else {
        return;
    };
    if agent.model_provider.trim().is_empty()
        && let Some(model_provider) = model_provider
    {
        agent.model_provider = model_provider.into();
    }
    if agent.risk_profile.trim().is_empty()
        && let Some(risk_profile) = risk_profile
    {
        agent.risk_profile = risk_profile;
    }
    if agent.runtime_profile.trim().is_empty()
        && let Some(runtime_profile) = runtime_profile
    {
        agent.runtime_profile = runtime_profile;
    }
}

fn mark_onboard_section_completed(cfg: &mut zeroclaw_config::schema::Config, section: &str) {
    if !cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|completed| completed == section)
    {
        cfg.onboard_state
            .completed_sections
            .push(section.to_string());
        cfg.mark_dirty("onboard-state.completed-sections");
    }
}

fn first_alias_preferring_default<'a>(aliases: impl Iterator<Item = &'a String>) -> Option<String> {
    let mut aliases: Vec<&String> = aliases.collect();
    aliases.sort();
    aliases
        .iter()
        .find(|alias| alias.as_str() == "default")
        .or_else(|| aliases.first())
        .map(|alias| (*alias).clone())
}

/// `tunnel`-flavored picker: same as `schema_walk_picker` plus a synthetic
/// `none` entry at the top, marked active when the current `tunnel.tunnel_provider`
/// matches. Mirrors the TUI's tunnel section.
fn schema_walk_picker_with_none(
    cfg: &zeroclaw_config::schema::Config,
    section: &str,
    active_prop_path: &str,
) -> Vec<PickerItem> {
    let active = cfg.get_prop(active_prop_path).unwrap_or_default();
    let mut items = vec![PickerItem {
        key: "none".to_string(),
        label: "none".to_string(),
        description: Some("Localhost only — no public tunnel.".to_string()),
        badge: if active == "none" || active.is_empty() {
            Some("active".to_string())
        } else {
            None
        },
    }];
    let mut rest = schema_walk_picker(cfg, section);
    // Re-mark the active one in the schema-walk results.
    for item in &mut rest {
        if item.key == active {
            item.badge = Some("active".to_string());
        }
    }
    items.extend(rest);
    items
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SelectItemResponse {
    /// The dotted prefix the frontend should use for `GET /api/config/list?prefix=...`
    /// to render the form for the selected item. E.g. picking `anthropic`
    /// under Providers returns `model_providers.anthropic`.
    pub fields_prefix: String,
    /// True if this select created a new entry (vs. resolved to an existing one).
    pub created: bool,
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionItemPath {
    pub section: String,
    pub key: String,
}

/// `POST /api/onboard/sections/<section>/items/<key>` — instantiate the
/// selected item in the live config (idempotent) and return the dotted
/// prefix the frontend should fetch fields under.
///
/// Per-section dispatch:
/// * `providers` → POST equivalent of `/api/config/map-key?path=providers.models&key=<key>`,
///   then return `model_providers.<key>`.
/// * `channels` → init_defaults under `channels.<key>`, return `channels.<key>`.
/// * `memory` → set_prop `memory.backend = <key>`, return `memory`.
/// * `tunnel` → set_prop `tunnel.tunnel_provider = <key>` (and init_defaults the
///   subsection if `<key>` is not "none"), return `tunnel.<key>` (or `tunnel`
///   for the `none` case).
///
/// The optional JSON body `{"alias": "<name>"}` names the entry being created,
/// e.g. `"work"` for `model_providers.anthropic.work`. Omit to use `"default"`.
#[derive(Debug, Default, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionSelectBody {
    pub alias: Option<String>,
}

fn typed_family_config_key(section: zeroclaw_config::sections::Section, key: &str) -> String {
    if matches!(
        section,
        zeroclaw_config::sections::Section::ModelProviders
            | zeroclaw_config::sections::Section::TtsProviders
            | zeroclaw_config::sections::Section::TranscriptionProviders
    ) {
        // Provider catalog/runtime identifiers use snake_case
        // (`atomic_chat`, `local_whisper`), while Configurable field paths
        // are kebab-case (`atomic-chat`, `local-whisper`). Keep references
        // snake_case; normalize only the config-map path used to create and
        // edit the alias block.
        key.replace('_', "-")
    } else {
        key.to_string()
    }
}

pub async fn handle_section_select(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(SectionItemPath { section, key }): axum::extract::Path<SectionItemPath>,
    body: Option<axum::extract::Json<SectionSelectBody>>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let alias = body
        .and_then(|b| b.0.alias)
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "default".to_string());

    let mut working = state.config.read().clone();

    use zeroclaw_config::sections::Section;
    let Some(section_enum) = Section::from_key(&section) else {
        return error_response(
            ConfigApiError::new(
                ConfigApiCode::PathNotFound,
                format!("no picker semantics defined for section `{section}`"),
            )
            .with_path(section.as_str()),
        );
    };

    let (fields_prefix, created) = match section_enum {
        Section::ModelProviders | Section::TtsProviders | Section::TranscriptionProviders => {
            // Two-tier typed-family path: outer bucket is the family
            // (`model_providers.<type>` etc.), inner key is the alias the
            // operator named. `create_map_key` is idempotent so re-selecting
            // an existing type/alias is a no-op for the bucket and just
            // returns the form prefix for the alias.
            let family = section_enum.as_str();
            let config_key = typed_family_config_key(section_enum, &key);
            let created = working
                .create_map_key(&format!("{family}.{config_key}"), &alias)
                .map_err(|msg| {
                    error_response(
                        ConfigApiError::new(
                            ConfigApiCode::PathNotFound,
                            format!("could not select {family} `{key}` alias `{alias}`: {msg}"),
                        )
                        .with_path(format!("{family}.{config_key}")),
                    )
                });
            let created = match created {
                Ok(c) => c,
                Err(resp) => return resp,
            };
            // Per-family typed configs derive their own default endpoint
            // URI via family traits at runtime construction time.
            (format!("{family}.{config_key}.{alias}"), created)
        }
        Section::Channels => {
            let created = working
                .create_map_key(&format!("channels.{key}"), &alias)
                .map_err(|msg| {
                    error_response(
                        ConfigApiError::new(
                            ConfigApiCode::PathNotFound,
                            format!("could not select channel `{key}` alias `{alias}`: {msg}"),
                        )
                        .with_path(format!("channels.{key}")),
                    )
                });
            let created = match created {
                Ok(c) => c,
                Err(resp) => return resp,
            };
            // The per-channel-type struct's `enabled` field defaults to
            // `false` (pre-v0.8.0 paste-safety rationale: don't fire a
            // listener on a half-pasted block). For wizard-driven creation
            // the operator has just consciously added the alias, so flip
            // the new entry's `enabled` to true. Re-selecting an existing
            // alias is a no-op (created=false), so user-edited values are
            // never trampled.
            if created {
                let enabled_path = format!("channels.{key}.{alias}.enabled");
                if let Err(e) = working.set_prop_persistent(&enabled_path, "true") {
                    ::zeroclaw_log::record!(
                        WARN,
                        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                            .with_attrs(
                                ::serde_json::json!({"path": enabled_path, "error": format!("{}", e)})
                            ),
                        "failed to default-enable newly created channel; operator must toggle manually"
                    );
                }
            }
            (format!("channels.{key}.{alias}"), created)
        }
        Section::Agents
        | Section::PeerGroups
        | Section::Cron
        | Section::McpBundles
        | Section::KnowledgeBundles
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles => {
            // OneTierAliasMap: the URL path key IS the alias. One
            // `create_map_key("<section>", &key)` call works for every
            // operator-named HashMap section; create_map_key is
            // idempotent, so selecting an existing alias just returns
            // the form prefix without modifying anything.
            let section_key = section_enum.as_str();
            let created = working.create_map_key(section_key, &key).map_err(|msg| {
                error_response(
                    ConfigApiError::new(
                        ConfigApiCode::PathNotFound,
                        format!("could not select {section_key} alias `{key}`: {msg}"),
                    )
                    .with_path(section_key),
                )
            });
            let created = match created {
                Ok(c) => c,
                Err(resp) => return resp,
            };
            // Agents need a per-alias workspace dir on disk so the
            // PersonalityEditor and the runtime have somewhere to read
            // and write IDENTITY.md / SOUL.md / USER.md / etc.
            if created && matches!(section_enum, Section::Agents) {
                apply_first_run_agent_defaults(&mut working, &key);
                let workspace_dir = working.agent_workspace_dir(&key);
                if let Err(err) = tokio::fs::create_dir_all(&workspace_dir).await {
                    return error_response(
                        ConfigApiError::new(
                            ConfigApiCode::ValidationFailed,
                            format!(
                                "created agent `{key}` but failed to scaffold workspace at {}: {err}",
                                workspace_dir.display()
                            ),
                        )
                        .with_path(section_key),
                    );
                }
                if let Err(err) =
                    zeroclaw_config::schema::ensure_bootstrap_files(&workspace_dir).await
                {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": key, "workspace": workspace_dir.display().to_string(), "err": err.to_string()})), "agent workspace scaffolded but bootstrap files seed failed (continuing)");
                }
            }
            (format!("{section_key}.{key}"), created)
        }
        Section::Storage => {
            // Two-tier typed-family (`storage.<kind>.<alias>`) — same
            // shape and selection flow as model_providers / tts_providers /
            // transcription_providers. Outer bucket is the storage kind
            // (sqlite, postgres, qdrant, markdown, lucid); inner key is
            // the operator-named alias.
            let created = working
                .create_map_key(&format!("storage.{key}"), &alias)
                .map_err(|msg| {
                    error_response(
                        ConfigApiError::new(
                            ConfigApiCode::PathNotFound,
                            format!("could not select storage `{key}` alias `{alias}`: {msg}"),
                        )
                        .with_path(format!("storage.{key}")),
                    )
                });
            let created = match created {
                Ok(c) => c,
                Err(resp) => return resp,
            };
            mark_onboard_section_completed(&mut working, "storage");
            (format!("storage.{key}.{alias}"), created)
        }
        Section::Memory => {
            // Set memory.backend to the picked key. Fields_prefix points at
            // `memory` so the form renders the whole memory section
            // (the active backend's specific fields show up there).
            if let Err(e) = working.set_prop_persistent("memory.backend", &key) {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("could not set memory.backend = `{key}`: {e}"),
                    )
                    .with_path("memory.backend"),
                );
            }
            mark_onboard_section_completed(&mut working, "memory");
            ("memory".to_string(), true)
        }
        Section::Tunnel => {
            if let Err(e) = working.set_prop_persistent("tunnel.tunnel-provider", &key) {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("could not set tunnel.tunnel-provider = `{key}`: {e}"),
                    )
                    .with_path("tunnel.tunnel-provider"),
                );
            }
            let prefix = if key == "none" {
                "tunnel".to_string()
            } else {
                let p = format!("tunnel.{key}");
                working.init_defaults(Some(&p));
                p
            };
            (prefix, true)
        }
        Section::Hardware | Section::Mcp | Section::Skills => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!(
                        "section `{}` is a direct-form section with no picker; \
                         render fields via GET /api/config/list?prefix={}",
                        section_enum, section_enum
                    ),
                )
                .with_path(section_enum.as_str()),
            );
        }
    };

    if created {
        working.mark_dirty(&fields_prefix);
    }

    if let Err(e) = working.save_dirty().await {
        return error_response(ConfigApiError::new(
            ConfigApiCode::ReloadFailed,
            format!("save after select failed: {e}"),
        ));
    }
    *state.config.write() = working;

    axum::Json(SelectItemResponse {
        fields_prefix,
        created,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression guard: every alias-bearing map the handler exposes must
    /// be reachable from `Config::get_map_keys` using the kebab-case path
    /// `build_agent_options` passes. Snake_case silently returns `None` →
    /// empty Vec → dashboard renders "No X configured yet" when X exists.
    /// This test drives the same code the gateway runs and would have
    /// caught the original bug. Adding a new alias-bearing field requires
    /// adding it here too.
    #[test]
    fn build_agent_options_returns_every_configured_alias() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.create_map_key("providers.models.anthropic", "default")
            .unwrap();
        cfg.create_map_key("risk-profiles", "alpha_risk").unwrap();
        cfg.create_map_key("runtime-profiles", "alpha_runtime")
            .unwrap();
        cfg.create_map_key("skill-bundles", "alpha_skills").unwrap();
        cfg.create_map_key("knowledge-bundles", "alpha_knowledge")
            .unwrap();
        cfg.create_map_key("mcp-bundles", "alpha_mcp").unwrap();
        cfg.create_map_key("agents", "alpha_agent").unwrap();

        let resp = build_agent_options(&cfg);

        assert_eq!(resp.model_providers, vec!["anthropic.default".to_string()]);
        assert_eq!(resp.risk_profiles, vec!["alpha_risk".to_string()]);
        assert_eq!(resp.runtime_profiles, vec!["alpha_runtime".to_string()]);
        assert_eq!(resp.skill_bundles, vec!["alpha_skills".to_string()]);
        assert_eq!(resp.knowledge_bundles, vec!["alpha_knowledge".to_string()],);
        assert_eq!(resp.mcp_bundles, vec!["alpha_mcp".to_string()]);
        assert_eq!(resp.agents, vec!["alpha_agent".to_string()]);
    }

    #[test]
    fn typed_provider_catalog_keys_can_create_kebab_config_sections() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        let cases = [
            (
                zeroclaw_config::sections::Section::ModelProviders,
                "providers.models",
                "atomic_chat",
                "providers.models.atomic-chat",
            ),
            (
                zeroclaw_config::sections::Section::ModelProviders,
                "providers.models",
                "gemini_cli",
                "providers.models.gemini-cli",
            ),
            (
                zeroclaw_config::sections::Section::TranscriptionProviders,
                "providers.transcription",
                "local_whisper",
                "providers.transcription.local-whisper",
            ),
        ];

        for (section, family, key, expected_path) in cases {
            let path = format!("{family}.{}", typed_family_config_key(section, key));
            assert_eq!(path, expected_path);
            cfg.create_map_key(&path, "default")
                .unwrap_or_else(|e| panic!("{key} should map to `{expected_path}`: {e}"));
        }

        assert!(
            cfg.providers.models.atomic_chat.contains_key("default"),
            "created Atomic Chat alias should land in the atomic_chat provider map",
        );
        assert!(
            cfg.providers.models.gemini_cli.contains_key("default"),
            "created Gemini CLI alias should land in the gemini_cli provider map",
        );
        assert!(
            cfg.providers
                .transcription
                .local_whisper
                .contains_key("default"),
            "created Local Whisper alias should land in the local_whisper provider map",
        );
    }

    #[test]
    fn derive_onboard_status_requires_dispatchable_agent() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        let resp = derive_onboard_status(&cfg);
        assert!(resp.needs_onboarding);
        assert_eq!(resp.reason, "fresh_install");

        cfg.create_map_key("providers.models.anthropic", "default")
            .unwrap();
        let resp = derive_onboard_status(&cfg);
        assert!(
            resp.needs_onboarding,
            "provider configured without a bound agent must not flip needs_onboarding"
        );
        assert_eq!(resp.reason, "incomplete_agent");
        assert!(resp.has_partial_state);

        cfg.create_map_key("risk-profiles", "default").unwrap();
        cfg.create_map_key("runtime-profiles", "default").unwrap();
        cfg.create_map_key("agents", "default").unwrap();
        let resp = derive_onboard_status(&cfg);
        assert!(
            resp.needs_onboarding,
            "agent without provider/profile bindings must still need onboarding"
        );
        assert_eq!(resp.reason, "incomplete_agent");
        assert!(
            resp.missing
                .iter()
                .any(|m| m == "Set a model provider for agent `default`.")
        );

        let agent = cfg.agents.get_mut("default").unwrap();
        agent.model_provider = "anthropic.default".into();
        agent.risk_profile = "default".into();
        agent.runtime_profile = "default".into();
        let resp = derive_onboard_status(&cfg);
        assert!(
            resp.needs_onboarding,
            "provider alias without a selected model must still need onboarding"
        );
        assert!(
            resp.missing
                .iter()
                .any(|m| m == "Choose a model for model provider `anthropic.default`.")
        );

        cfg.set_prop_persistent("providers.models.anthropic.default.model", "claude-sonnet")
            .unwrap();
        let resp = derive_onboard_status(&cfg);
        assert!(
            resp.needs_onboarding,
            "hosted provider alias without credential/auth must still need onboarding"
        );
        assert!(
            resp.missing
                .iter()
                .any(|m| m == "Set credential/auth for model provider `anthropic.default`.")
        );

        cfg.set_prop_persistent("providers.models.anthropic.default.api-key", "sk-test")
            .unwrap();
        let resp = derive_onboard_status(&cfg);
        assert!(!resp.needs_onboarding);
        assert_eq!(resp.reason, "has_dispatchable_agent");
        assert!(!resp.has_partial_state || resp.missing.is_empty());
    }

    #[test]
    fn derive_onboard_status_completed_sections_without_dispatchable_agent_stays_pending() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.onboard_state
            .completed_sections
            .push("providers.models".into());
        let resp = derive_onboard_status(&cfg);
        assert!(
            resp.needs_onboarding,
            "completed_sections marker without a dispatchable agent must NOT flip the redirect"
        );
        assert_eq!(resp.reason, "incomplete_agent");
    }

    #[test]
    fn apply_first_run_agent_defaults_binds_existing_provider_and_profiles() {
        let mut cfg = zeroclaw_config::schema::Config::default();
        cfg.create_map_key("providers.models.anthropic", "work")
            .unwrap();
        cfg.create_map_key("risk-profiles", "default").unwrap();
        cfg.create_map_key("runtime-profiles", "deep_work").unwrap();
        cfg.create_map_key("agents", "default").unwrap();

        apply_first_run_agent_defaults(&mut cfg, "default");

        let agent = cfg.agents.get("default").unwrap();
        assert_eq!(agent.model_provider.as_str(), "anthropic.work");
        assert_eq!(agent.risk_profile, "default");
        assert_eq!(agent.runtime_profile, "deep_work");
    }

    #[test]
    fn memory_section_ready_tracks_onboarding_progress_not_default_backend() {
        let cfg = zeroclaw_config::schema::Config::default();
        assert!(
            !section_ready(&cfg, "memory", false),
            "fresh onboarding should not show Memory checked merely because a default backend exists"
        );
        assert!(
            section_ready(&cfg, "memory", true),
            "Memory should show checked after the user has advanced through that section"
        );
    }

    fn empty_cfg() -> zeroclaw_config::schema::Config {
        zeroclaw_config::schema::Config::default()
    }

    #[test]
    fn handle_sections_derives_every_top_level_field_from_schema() {
        // Regression: the section list must be schema-driven, not the old
        // hardcoded 6. Adding a new top-level field to `Config` should make
        // it appear here automatically.
        let cfg = empty_cfg();
        let mut roots: std::collections::BTreeSet<String> = cfg
            .prop_fields()
            .iter()
            .filter_map(|f| f.name.split('.').next().map(str::to_string))
            .collect();
        // Mirror handle_sections: map-keyed sections surface even when
        // their HashMap is empty (prop_fields only emits paths for
        // populated entries).
        for s in zeroclaw_config::schema::Config::map_key_sections() {
            if let Some(first) = s.path.split('.').next() {
                roots.insert(first.to_string());
            }
        }
        for hidden in HIDDEN_TOP_LEVEL {
            roots.remove(*hidden);
        }
        // The 5 onboarding sections must still be in the derived set.
        for required in ["providers", "channels", "memory", "hardware", "tunnel"] {
            assert!(
                roots.contains(required),
                "derived sections must include onboarding section `{required}`; got {roots:?}",
            );
        }
        // Plus a sample of the runtime sections that used to be invisible.
        for runtime in ["gateway", "observability", "scheduler", "security"] {
            assert!(
                roots.contains(runtime),
                "derived sections must include runtime section `{runtime}`; got {roots:?}",
            );
        }
        // System / housekeeping fields must NOT surface.
        for hidden in HIDDEN_TOP_LEVEL {
            assert!(
                !roots.contains(*hidden),
                "hidden top-level `{hidden}` must not appear",
            );
        }
        for hidden in ["onboard_state", "onboard-state"] {
            assert!(
                !roots.contains(hidden),
                "onboarding bookkeeping root `{hidden}` must not appear",
            );
        }
    }

    #[test]
    fn channels_select_initializes_subsection_so_set_prop_works() {
        // Regression for the channels init/set flow: after
        // handle_section_select for channels/matrix, the in-memory config
        // must have channels.matrix.<alias> so a subsequent set_prop on
        // channels.matrix.* succeeds rather than bailing "Unknown property".
        // Uses create_map_key directly (the synchronous core of the select
        // endpoint) to keep the test free of HTTP machinery.
        let mut cfg = empty_cfg();
        assert!(cfg.channels.matrix.is_empty(), "fresh config: matrix unset");

        cfg.create_map_key("channels.matrix", "mymatrixalias")
            .expect("create_map_key must succeed for channels.matrix");
        assert!(
            cfg.channels.matrix.contains_key("mymatrixalias"),
            "channels.matrix must have alias after create_map_key",
        );

        // The form would issue a PATCH whose set_prop call hits this path.
        cfg.set_prop(
            "channels.matrix.mymatrixalias.allowed-rooms",
            r#"["alice","bob"]"#,
        )
        .expect("set_prop on initialized matrix subsection must succeed");
        assert_eq!(
            cfg.channels
                .matrix
                .get("mymatrixalias")
                .unwrap()
                .allowed_rooms,
            vec!["alice".to_string(), "bob".to_string()],
        );
    }

    #[test]
    fn providers_picker_sources_from_list_providers() {
        // Single source of truth: zeroclaw_providers::list_model_providers().
        // Anthropic / OpenAI / OpenRouter must surface in the picker.
        let cfg = empty_cfg();
        let items = providers_picker(&cfg);
        let names: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert!(
            names.contains(&"anthropic"),
            "expected anthropic in picker, got: {names:?}"
        );
        assert!(names.contains(&"openai"), "expected openai in picker");
        assert!(
            names.contains(&"openrouter"),
            "expected openrouter in picker"
        );

        // Display name is human-readable, not the canonical key.
        let anthropic = items.iter().find(|i| i.key == "anthropic").unwrap();
        assert_eq!(anthropic.label, "Anthropic");

        // Local-only model_providers carry a description hint.
        let local = items.iter().find(|i| i.description.is_some());
        assert!(
            local.is_some(),
            "at least one model_provider should be marked local"
        );

        // Empty config has no model_provider aliases — no badges yet.
        assert!(
            items.iter().all(|i| i.badge.is_none()),
            "fresh config shouldn't mark any model_provider as present"
        );
    }

    #[test]
    fn providers_picker_marks_alias_readiness() {
        // Typed-family layout: each canonical family is a map-keyed
        // sub-section at `model_providers.<family>` whose entries are
        // operator-named aliases. Creating the alias alone is not enough
        // for chat dispatch; it still needs model + credential/auth.
        let mut cfg = empty_cfg();
        cfg.create_map_key("providers.models.anthropic", "default")
            .expect("create_map_key");
        let items = providers_picker(&cfg);
        let anthropic = items.iter().find(|i| i.key == "anthropic").unwrap();
        assert_eq!(
            anthropic.badge.as_deref(),
            Some("needs setup"),
            "anthropic should need setup after adding an empty alias"
        );

        cfg.set_prop(
            "providers.models.anthropic.default.model",
            "claude-sonnet-4-5",
        )
        .expect("set model");
        cfg.set_prop("providers.models.anthropic.default.api-key", "sk-test")
            .expect("set api key");
        let items = providers_picker(&cfg);
        let anthropic = items.iter().find(|i| i.key == "anthropic").unwrap();
        assert_eq!(
            anthropic.badge.as_deref(),
            Some("configured"),
            "anthropic should be marked configured once required chat fields are present"
        );
    }

    #[test]
    fn memory_picker_sources_from_selectable_backends() {
        let cfg = empty_cfg();
        let items = memory_picker(&cfg);
        // Mirrors zeroclaw_memory::selectable_memory_backends() exactly.
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert!(keys.contains(&"sqlite"));
        assert!(keys.contains(&"none"));
        // Fresh onboarding should not imply the user selected the default.
        let active = items.iter().find(|i| i.badge.as_deref() == Some("active"));
        assert!(
            active.is_none(),
            "fresh onboarding should not mark a memory backend active before the user confirms the step"
        );
    }

    #[test]
    fn channels_picker_walks_schema_via_init_defaults() {
        // Pure schema discovery — same trick the TUI uses. Whatever channels
        // the build has compiled in (matrix / discord / slack / etc.) appears
        // in the picker without any hand-maintained list. Test asserts a
        // representative sample compiled into the default `ci-all` build.
        let cfg = empty_cfg();
        let items = schema_walk_picker(&cfg, "channels");
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        assert!(!keys.is_empty(), "channel picker must not be empty");
        // Channels that are unconditionally compiled (no feature gate)
        // should always appear:
        for expected in ["telegram", "slack", "discord"] {
            assert!(
                keys.contains(&expected),
                "expected `{expected}` in channels picker, got: {keys:?}"
            );
        }
        // Fresh config — nothing configured yet.
        assert!(
            items.iter().all(|i| i.badge.is_none()),
            "fresh config shouldn't mark any channel as configured"
        );
    }

    #[test]
    fn channels_picker_marks_configured_after_create_map_key() {
        let mut cfg = empty_cfg();
        cfg.create_map_key("channels.matrix", "mymatrixalias")
            .expect("create_map_key must succeed for channels.matrix");
        let items = schema_walk_picker(&cfg, "channels");
        let matrix = items.iter().find(|i| i.key == "matrix").unwrap();
        assert_eq!(
            matrix.badge.as_deref(),
            Some("configured"),
            "matrix should be marked configured after create_map_key"
        );
    }

    #[test]
    fn tunnel_picker_includes_synthetic_none() {
        let cfg = empty_cfg();
        let items = schema_walk_picker_with_none(&cfg, "tunnel", "tunnel.tunnel-provider");
        assert_eq!(
            items[0].key, "none",
            "`none` must be the first entry in the tunnel picker"
        );
        // `none` is the active default for a fresh config.
        assert_eq!(items[0].badge.as_deref(), Some("active"));
    }

    /// Empty OneTierAliasMap section yields zero picker items. No
    /// closed-kind catalog applies for these sections — only operator-defined
    /// aliases populate the picker. Section wire keys are kebab-case
    /// because the Configurable derive runs each field name through
    /// `snake_to_kebab` when registering map-key paths.
    #[test]
    fn one_tier_alias_map_picker_is_empty_for_unconfigured_section() {
        let cfg = empty_cfg();
        for section in [
            "peer-groups",
            "cron",
            "mcp-bundles",
            "knowledge-bundles",
            "skill-bundles",
            "risk-profiles",
            "runtime-profiles",
        ] {
            let items = one_tier_alias_map_picker(&cfg, section);
            assert!(
                items.is_empty(),
                "`{section}` picker must be empty on a fresh config, got: {:?}",
                items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            );
        }
    }

    /// After `create_map_key("<kebab-section>", "<alias>")`, the picker
    /// surfaces the alias as a `configured` entry. Same shape applies
    /// to every OneTierAliasMap section — the picker is generic over
    /// the prefix.
    #[test]
    fn one_tier_alias_map_picker_surfaces_created_aliases() {
        let cases: &[(&str, &str)] = &[
            ("peer-groups", "team_chat"),
            ("cron", "daily_brief"),
            ("mcp-bundles", "core_tools"),
            ("knowledge-bundles", "house_docs"),
            ("skill-bundles", "ops_skills"),
            ("risk-profiles", "tight"),
            ("runtime-profiles", "fast_model"),
        ];
        for (section, alias) in cases {
            let mut cfg = empty_cfg();
            cfg.create_map_key(section, alias)
                .unwrap_or_else(|e| panic!("create_map_key({section}, {alias}) failed: {e}"));
            let items = one_tier_alias_map_picker(&cfg, section);
            assert!(
                items.iter().any(|i| i.key == *alias),
                "`{section}` picker should surface `{alias}` after create_map_key; got: {:?}",
                items.iter().map(|i| i.key.as_str()).collect::<Vec<_>>(),
            );
            let entry = items.iter().find(|i| i.key == *alias).unwrap();
            assert_eq!(
                entry.badge.as_deref(),
                Some("configured"),
                "`{section}.{alias}` should be badged `configured`",
            );
        }
    }

    /// Exhaustive picker dispatch: every [`Section`] variant must
    /// resolve through `picker_items_for` without panic. DirectForm
    /// sections (Workspace, Hardware, Mcp) return the
    /// `PickerDispatch::DirectForm` sentinel; every other section
    /// returns at least zero items. Loops over the wizard order plus
    /// every explorer-only variant — adding a new Section variant
    /// fails to compile until it gets a routing arm in
    /// `picker_items_for`.
    #[test]
    fn picker_dispatch_covers_every_section_variant() {
        use zeroclaw_config::sections::Section;
        let cfg = empty_cfg();
        // The full Section surface = wizard steps + explorer-only.
        // Spelling them out here pins both groups, so adding a row to
        // the `sections!` macro forces an update here too.
        let all: &[Section] = &[
            Section::ModelProviders,
            Section::TtsProviders,
            Section::TranscriptionProviders,
            Section::Channels,
            Section::Memory,
            Section::Hardware,
            Section::Tunnel,
            Section::Agents,
            Section::PeerGroups,
            Section::Storage,
            Section::Cron,
            Section::Mcp,
            Section::McpBundles,
            Section::KnowledgeBundles,
            Section::SkillBundles,
            Section::RiskProfiles,
            Section::RuntimeProfiles,
        ];
        let direct_form = [Section::Hardware, Section::Mcp];
        for section in all {
            match picker_items_for(*section, &cfg) {
                PickerDispatch::Items(_items) => {
                    assert!(
                        !direct_form.contains(section),
                        "{section:?} is marked DirectForm but dispatched to Items",
                    );
                }
                PickerDispatch::DirectForm => {
                    assert!(
                        direct_form.contains(section),
                        "{section:?} returned DirectForm but is not in the DirectForm set; \
                         either give it a picker arm or add it to the DirectForm list",
                    );
                }
            }
        }
    }

    /// Storage is `[storage.<kind>.<alias>]` — two-tier typed-family
    /// shape, served by the storage picker. The picker
    /// surfaces the 5 storage kinds (sqlite, postgres, qdrant,
    /// markdown, lucid) regardless of which aliases exist, and badges
    /// the kind `created` once any alias under it is created.
    #[test]
    fn storage_picker_lists_all_kinds_and_marks_created() {
        let cfg = empty_cfg();
        let items = storage_picker(&cfg);
        let keys: Vec<&str> = items.iter().map(|i| i.key.as_str()).collect();
        for expected in ["sqlite", "postgres", "qdrant", "markdown", "lucid"] {
            assert!(
                keys.contains(&expected),
                "storage picker must list `{expected}`, got: {keys:?}",
            );
        }
        // Fresh config — no kind should be badged.
        assert!(
            items.iter().all(|i| i.badge.is_none()),
            "fresh config: no storage kind should be marked configured",
        );

        // Create a sqlite instance; the sqlite row should flip to configured.
        let mut cfg2 = empty_cfg();
        cfg2.create_map_key("storage.sqlite", "primary")
            .expect("create_map_key(storage.sqlite, primary) must succeed");
        let items = storage_picker(&cfg2);
        let sqlite = items.iter().find(|i| i.key == "sqlite").unwrap();
        assert_eq!(
            sqlite.badge.as_deref(),
            Some("created"),
            "storage.sqlite should be marked created after adding an alias",
        );
        assert!(
            sqlite.description.is_some(),
            "storage picker should explain each backend tradeoff",
        );
    }
}
