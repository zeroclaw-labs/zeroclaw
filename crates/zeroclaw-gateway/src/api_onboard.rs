//! Onboard catalog endpoint — exposes the provider + model catalog the CLI
//! wizard already uses, so the dashboard's "+ Add provider" affordance and
//! model-picker dropdown share the same source of truth as the CLI.
//!
//! No catalog data is hand-maintained at this layer. `list_providers()` lives
//! in `zeroclaw-providers` and is the canonical list; `list_models()` per
//! provider fetches from models.dev (cached) or the provider's own /models
//! endpoint. Same code paths as the CLI wizard.
//!
//! Issue #6175.

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
pub struct CatalogProvider {
    /// Canonical provider name as used in `[providers.models.<name>]`.
    pub name: String,
    /// Human-readable display name.
    pub display_name: String,
    /// Whether the provider is fully local (no API key required).
    pub local: bool,
    /// Aliases the provider also responds to (informational).
    pub aliases: Vec<String>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct CatalogResponse {
    pub providers: Vec<CatalogProvider>,
}

/// `GET /api/onboard/catalog` — list every provider the CLI wizard knows
/// about. The dashboard shows these in the "+ Add provider" picker so
/// CLI / web stay in sync.
pub async fn handle_catalog(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let _ = state;

    let providers: Vec<CatalogProvider> = zeroclaw_providers::list_providers()
        .into_iter()
        .map(|p| CatalogProvider {
            name: p.name.to_string(),
            display_name: p.display_name.to_string(),
            local: p.local,
            aliases: p.aliases.iter().map(|s| s.to_string()).collect(),
        })
        .collect();

    axum::Json(CatalogResponse { providers }).into_response()
}

#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsQuery {
    /// Provider name (canonical, from CatalogProvider.name).
    pub provider: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct ModelsResponse {
    pub provider: String,
    pub models: Vec<String>,
    /// `true` when the catalog was fetched live; `false` if the cache was
    /// served (or if this provider has no remote catalog and the empty list
    /// is the genuine answer).
    pub live: bool,
}

/// `GET /api/onboard/catalog/models?provider=<name>` — fetch the model list
/// for one provider. Same code path the CLI wizard uses
/// (`zeroclaw_providers::create_provider(...).list_models()`), which goes
/// through the models.dev cached catalog for OpenAI / Anthropic / Gemini,
/// the live `/v1/models` endpoint for OpenRouter, etc.
///
/// Lazy: the dashboard hits this only when the user picks a provider, so
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

    let handle = match zeroclaw_providers::create_provider(&q.provider, None) {
        Ok(h) => h,
        Err(e) => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!("unknown provider `{}`: {e}", q.provider),
                )
                .with_path(&q.provider),
            );
        }
    };

    let (models, live) = match handle.list_models().await {
        Ok(m) => (m, true),
        Err(e) => {
            tracing::debug!(provider = %q.provider, error = ?e, "model catalog fetch failed");
            (Vec::new(), false)
        }
    };

    axum::Json(ModelsResponse {
        provider: q.provider,
        models,
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
    /// Stable section key — `workspace`, `providers`, `channels`, `memory`,
    /// `hardware`, `tunnel`. Matches `Section::as_path_prefix` in
    /// zeroclaw-runtime so CLI / web stay aligned.
    pub key: String,
    /// Human-readable section name for headers / breadcrumbs.
    pub label: String,
    /// Help text the wizard shows under the section title.
    pub help: String,
    /// `true` when this section requires picking an item before the form
    /// renders (Providers / Channels / Memory / Tunnel). `false` for sections
    /// that have a single direct form (Workspace / Hardware).
    pub has_picker: bool,
    /// Whether the user has marked the section completed in
    /// `onboard_state.completed_sections`.
    pub completed: bool,
    /// Display group for the dashboard sidebar (`Foundation`, `Agent`,
    /// `Tools`, etc.). Curated server-side until v3 / #5947 lands a schema
    /// attribute that encodes the grouping declaratively.
    pub group: String,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionsResponse {
    pub sections: Vec<SectionInfo>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct OnboardStatusResponse {
    /// `true` when the user hasn't started onboarding yet — no completed
    /// section markers AND no usable provider configured. The dashboard
    /// uses this signal to redirect first-load visits from `/` to
    /// `/onboard`.
    pub needs_onboarding: bool,
    /// Short machine-readable reason for the value of `needs_onboarding`,
    /// for logs / debugging. Stable: `fresh_install` / `has_provider` /
    /// `has_completed_sections`.
    pub reason: &'static str,
}

/// `GET /api/onboard/status` — boolean signal for the dashboard's
/// fresh-install redirect. The daemon writes a default `config.toml` on
/// first init, so file existence isn't a useful "is the user new?" check.
/// Instead we look at two explicit user-driven markers: any
/// `onboard_state.completed_sections` entry (set when the wizard finishes
/// a section) OR any usable provider (`providers.fallback` set, or any
/// entry under `providers.models`). When neither is present, the user is
/// fresh and should land at `/onboard` instead of the empty Dashboard.
pub async fn handle_onboard_status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.lock().clone();

    let has_completed = !cfg.onboard_state.completed_sections.is_empty();
    let has_provider = !cfg.providers.fallback.is_empty() || !cfg.providers.models.is_empty();

    let (needs_onboarding, reason) = if has_completed {
        (false, "has_completed_sections")
    } else if has_provider {
        (false, "has_provider")
    } else {
        (true, "fresh_install")
    };

    axum::Json(OnboardStatusResponse {
        needs_onboarding,
        reason,
    })
    .into_response()
}

/// `GET /api/onboard/sections` — list every top-level config section.
///
/// Schema-driven: walks `Config::prop_fields()` and collects unique first
/// segments, then asks `Config::map_key_sections()` for which ones have
/// pickers. The 4 onboarding sections (`providers`, `channels`, `memory`,
/// `tunnel`) keep their existing per-section dispatch in
/// `handle_section_picker`; everything else (`gateway`, `observability`,
/// `scheduler`, ...) renders as a direct form. Adding a new top-level
/// field to `Config` makes it appear here automatically.
pub async fn handle_sections(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.lock().clone();
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

    // Map-keyed roots get pickers automatically (the picker shows existing
    // keys / catalog entries; selecting an item opens its form).
    let map_keyed_roots: std::collections::HashSet<&'static str> =
        zeroclaw_config::schema::Config::map_key_sections()
            .iter()
            .filter_map(|s| s.path.split('.').next())
            .collect();

    let sections: Vec<SectionInfo> = roots
        .into_iter()
        .map(|key| {
            let has_picker = SECTIONS_WITH_PICKER.contains(&key.as_str())
                || map_keyed_roots.contains(key.as_str());
            SectionInfo {
                completed: completed.contains(&key),
                label: humanize_section(&key),
                help: section_help(&key).to_string(),
                has_picker,
                group: section_group(&key).to_string(),
                key,
            }
        })
        .collect();

    axum::Json(SectionsResponse { sections }).into_response()
}

/// Top-level fields that exist on `Config` but are never user-editable
/// from the dashboard (schema bookkeeping, resolved at runtime).
const HIDDEN_TOP_LEVEL: &[&str] = &["schema-version", "onboard-state"];

/// Sections whose picker semantics are non-generic and live in the
/// per-section dispatch in `handle_section_picker` (catalog of providers,
/// memory backend list, tunnel-with-none, channel sub-table walk).
const SECTIONS_WITH_PICKER: &[&str] = &["providers", "channels", "memory", "tunnel"];

/// Humanize a section key for display (`google_workspace` → `Google workspace`).
/// Keeps things simple and predictable; specific wording overrides go in
/// the section-help table or per-section labels if/when we add them.
fn humanize_section(key: &str) -> String {
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
        // The 6 foundation sections (TUI's `Section` enum) — every install
        // touches these. Named for the role they play, not for the wizard
        // that happens to walk them on first run.
        "workspace" | "providers" | "channels" | "memory" | "hardware" | "tunnel" => "Foundation",
        // Agent loop, scheduling, and orchestration.
        "agent"
        | "autonomy"
        | "cron"
        | "heartbeat"
        | "hooks"
        | "pacing"
        | "pipeline"
        | "query_classification"
        | "reliability"
        | "runtime"
        | "scheduler"
        | "skills"
        | "sop"
        | "verifiable_intent" => "Agent",
        // Multi-agent / delegation.
        "agents" | "swarms" | "delegate" => "Multi-agent",
        // Tool integrations.
        "browser" | "browser_delegate" | "http_request" | "image_gen" | "knowledge"
        | "link_enricher" | "mcp" | "media_pipeline" | "multimodal" | "plugins"
        | "project_intel" | "shell_tool" | "text_browser" | "transcription" | "tts"
        | "web_fetch" | "web_search" => "Tools",
        // External services / vendor integrations.
        "claude_code" | "claude_code_runner" | "codex_cli" | "composio" | "gemini_cli"
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

/// Help text for a section. Curated copy for the onboarding sections;
/// empty string for everything else (the form's title is enough until
/// someone writes copy).
fn section_help(key: &str) -> &'static str {
    match key {
        "workspace" => {
            "Where ZeroClaw stores its config and runtime data. Defaults work for most setups."
        }
        "providers" => {
            "Paste an API key (e.g. `sk-ant-...` for Anthropic, `sk-...` for OpenAI) when prompted. \
                        For OAuth-based providers run: zeroclaw auth login --provider <name>"
        }
        "channels" => {
            "Pick which chat platforms ZeroClaw should listen on. You can configure multiple."
        }
        "memory" => "Persistent memory backend. SQLite is recommended; pick `none` to disable.",
        "hardware" => {
            "Optional: hardware peripherals (Arduino, STM32, GPIO, etc.). Skip if you don't need them."
        }
        "tunnel" => {
            "Optional: expose your gateway over the public internet via Cloudflare or ngrok. \
                     Pick `none` to keep it localhost-only."
        }
        _ => "",
    }
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
    /// extended description, "(local)" for local-only providers).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Optional badge — `"configured"` when an entry already exists for
    /// this item under the section's path. The frontend uses this to mark
    /// the row distinct so users see what they've already done.
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
/// * `providers` → `zeroclaw_providers::list_providers()` (CLI's catalog).
/// * `memory` → `zeroclaw_memory::selectable_memory_backends()`.
/// * `channels` / `tunnel` → schema-walk: clone config, `init_defaults` the
///   section, then strip the section prefix from `prop_fields()` and dedupe
///   by first segment. Same trick the TUI uses; new channels appear
///   automatically when a `#[nested] Option<...>` field is added.
/// * Anything else returns 404 (workspace/hardware have no picker).
pub async fn handle_section_picker(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(SectionPath { section }): axum::extract::Path<SectionPath>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let cfg = state.config.lock().clone();

    let (items, help) = match section.as_str() {
        "providers" => (
            providers_picker(&cfg),
            section_help("providers").to_string(),
        ),
        "memory" => (memory_picker(&cfg), section_help("memory").to_string()),
        "channels" => (
            schema_walk_picker(&cfg, "channels"),
            section_help("channels").to_string(),
        ),
        "tunnel" => (
            schema_walk_picker_with_none(&cfg, "tunnel", "tunnel.provider"),
            section_help("tunnel").to_string(),
        ),
        other => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!(
                        "no picker for section `{other}`; \
                         workspace and hardware are direct-form sections \
                         (use GET /api/config/list?prefix=<section>)"
                    ),
                )
                .with_path(other),
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

fn providers_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    zeroclaw_providers::list_providers()
        .into_iter()
        .map(|p| {
            let configured = cfg.providers.models.contains_key(p.name);
            PickerItem {
                key: p.name.to_string(),
                label: p.display_name.to_string(),
                description: if p.local {
                    Some("Local — no API key required".to_string())
                } else {
                    None
                },
                badge: if configured {
                    Some("configured".to_string())
                } else {
                    None
                },
            }
        })
        .collect()
}

fn memory_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    let current = cfg.memory.backend.clone();
    zeroclaw_memory::selectable_memory_backends()
        .iter()
        .map(|b| PickerItem {
            key: b.key.to_string(),
            label: b.label.to_string(),
            description: None,
            badge: if b.key == current {
                Some("active".to_string())
            } else {
                None
            },
        })
        .collect()
}

/// Generic schema-walk picker for sections like `channels` whose subsections
/// are `#[nested] Option<T>` fields. Discovery: clone the config,
/// init_defaults the section to materialize every Option<T> as Some(default),
/// then read prop_fields() on the probe — every reachable subsection's name
/// surfaces as a path segment under `<section>.`.
fn schema_walk_picker(cfg: &zeroclaw_config::schema::Config, section: &str) -> Vec<PickerItem> {
    let mut probe = cfg.clone();
    probe.init_defaults(Some(section));
    let prefix_with_dot = format!("{section}.");

    let configured: std::collections::BTreeSet<String> = cfg
        .prop_fields()
        .iter()
        .filter_map(|f| f.name.strip_prefix(&prefix_with_dot))
        .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
        .collect();

    let all: std::collections::BTreeSet<String> = probe
        .prop_fields()
        .iter()
        .filter_map(|f| f.name.strip_prefix(&prefix_with_dot))
        .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
        .collect();

    all.into_iter()
        .map(|name| {
            // Two-tier badge: `configured` = a block exists on disk for this
            // item (auto-created on click + persisted). `active` = the block
            // exists AND its `enabled` field is currently `true`. Items
            // without an `enabled` field stay at `configured`. Surfaces the
            // "is this actually doing anything?" distinction the contributor
            // feedback on PR #6179 asked for, without changing init-on-click
            // semantics (the macro-level fix lands in schema v3 / #5947).
            let enabled_path = format!("{prefix_with_dot}{name}.enabled");
            let is_active = cfg.get_prop(&enabled_path).ok().as_deref() == Some("true");
            let badge = if is_active {
                Some("active".to_string())
            } else if configured.contains(&name) {
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

/// `tunnel`-flavored picker: same as `schema_walk_picker` plus a synthetic
/// `none` entry at the top, marked active when the current `tunnel.provider`
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
    /// under Providers returns `providers.models.anthropic`.
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
///   then return `providers.models.<key>`.
/// * `channels` → init_defaults under `channels.<key>`, return `channels.<key>`.
/// * `memory` → set_prop `memory.backend = <key>`, return `memory`.
/// * `tunnel` → set_prop `tunnel.provider = <key>` (and init_defaults the
///   subsection if `<key>` is not "none"), return `tunnel.<key>` (or `tunnel`
///   for the `none` case).
pub async fn handle_section_select(
    State(state): State<AppState>,
    headers: HeaderMap,
    axum::extract::Path(SectionItemPath { section, key }): axum::extract::Path<SectionItemPath>,
) -> Response {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let mut working = state.config.lock().clone();

    let (fields_prefix, created) = match section.as_str() {
        "providers" => {
            let created = working
                .create_map_key("providers.models", &key)
                .map_err(|msg| {
                    error_response(
                        ConfigApiError::new(
                            ConfigApiCode::PathNotFound,
                            format!("could not select provider `{key}`: {msg}"),
                        )
                        .with_path("providers.models"),
                    )
                });
            let created = match created {
                Ok(c) => c,
                Err(resp) => return resp,
            };
            // Pre-populate the provider's trait-level defaults (base_url for
            // Ollama, default temperature / max_tokens / etc.) so the form
            // opens with sensible values instead of a sea of empty inputs.
            // Idempotent — only fills paths that are still unset, so a user
            // who's already overridden a field doesn't get clobbered on
            // re-select.
            let prefix = format!("providers.models.{key}");
            if let Err(e) =
                zeroclaw_runtime::onboard::field_visibility::apply_provider_trait_defaults(
                    &mut working,
                    &key,
                    &prefix,
                )
            {
                tracing::warn!(provider = %key, error = ?e, "failed to apply trait defaults; form will start blank");
            }
            // Make the picked provider the runtime fallback so chat actually
            // routes to it. Without this, picking Ollama in onboarding would
            // create the entry but the chat path keeps using the prior
            // fallback (or fails because none is set), and the user lands
            // on "OpenRouter API key not set" trying to chat with a model
            // they thought they'd configured. Mirrors `zeroclaw onboard`'s
            // post-pick semantics.
            if let Err(e) = working.set_prop("providers.fallback", &key) {
                tracing::warn!(provider = %key, error = %e, "failed to set providers.fallback after pick");
            }
            (prefix, created)
        }
        "channels" => {
            let prefix = format!("channels.{key}");
            // init_defaults instantiates the subsection if it doesn't exist.
            // The set returned tells us whether something was newly created.
            let initialized = working.init_defaults(Some(&prefix));
            (prefix, !initialized.is_empty())
        }
        "memory" => {
            // Set memory.backend to the picked key. Fields_prefix points at
            // `memory` so the form renders the whole memory section
            // (the active backend's specific fields show up there).
            if let Err(e) = working.set_prop("memory.backend", &key) {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("could not set memory.backend = `{key}`: {e}"),
                    )
                    .with_path("memory.backend"),
                );
            }
            ("memory".to_string(), true)
        }
        "tunnel" => {
            if let Err(e) = working.set_prop("tunnel.provider", &key) {
                return error_response(
                    ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("could not set tunnel.provider = `{key}`: {e}"),
                    )
                    .with_path("tunnel.provider"),
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
        other => {
            return error_response(
                ConfigApiError::new(
                    ConfigApiCode::PathNotFound,
                    format!("no picker semantics defined for section `{other}`"),
                )
                .with_path(other),
            );
        }
    };

    if let Err(e) = working.save().await {
        return error_response(ConfigApiError::new(
            ConfigApiCode::ReloadFailed,
            format!("save after select failed: {e}"),
        ));
    }
    *state.config.lock() = working;

    axum::Json(SelectItemResponse {
        fields_prefix,
        created,
    })
    .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

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
        for hidden in HIDDEN_TOP_LEVEL {
            roots.remove(*hidden);
        }
        // The 6 onboarding sections must still be in the derived set.
        for required in [
            "workspace",
            "providers",
            "channels",
            "memory",
            "hardware",
            "tunnel",
        ] {
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
    }

    #[test]
    fn channels_select_initializes_subsection_so_set_prop_works() {
        // Regression for the channels init/set flow: after
        // handle_section_select for channels/matrix, the in-memory config
        // must have channels.matrix = Some(...) so a subsequent set_prop on
        // channels.matrix.* succeeds rather than bailing "Unknown property".
        // Calls init_defaults directly (the synchronous core of the select
        // endpoint) to keep the test free of HTTP machinery.
        let mut cfg = empty_cfg();
        assert!(cfg.channels.matrix.is_empty(), "fresh config: matrix unset");

        let initialized = cfg.init_defaults(Some("channels.matrix"));
        assert!(
            initialized.contains(&"channels.matrix"),
            "init_defaults must report channels.matrix initialized; got: {initialized:?}",
        );
        assert!(
            cfg.channels.matrix.contains_key("default"),
            "channels.matrix must have default after init_defaults",
        );

        // The form would issue a PATCH whose set_prop call hits this path.
        cfg.set_prop(
            "channels.matrix.default.allowed-rooms",
            r#"["alice","bob"]"#,
        )
        .expect("set_prop on initialized matrix subsection must succeed");
        assert_eq!(
            cfg.channels.matrix.get("default").unwrap().allowed_rooms,
            vec!["alice".to_string(), "bob".to_string()],
        );
    }

    #[test]
    fn providers_picker_sources_from_list_providers() {
        // Single source of truth: zeroclaw_providers::list_providers().
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

        // Local-only providers carry a description hint.
        let local = items.iter().find(|i| i.description.is_some());
        assert!(
            local.is_some(),
            "at least one provider should be marked local"
        );

        // Empty config has no configured providers — no badges yet.
        assert!(
            items.iter().all(|i| i.badge.is_none()),
            "fresh config shouldn't mark any provider as configured"
        );
    }

    #[test]
    fn providers_picker_marks_configured_after_create_map_key() {
        let mut cfg = empty_cfg();
        cfg.create_map_key("providers.models", "anthropic")
            .expect("create_map_key");
        let items = providers_picker(&cfg);
        let anthropic = items.iter().find(|i| i.key == "anthropic").unwrap();
        assert_eq!(
            anthropic.badge.as_deref(),
            Some("configured"),
            "anthropic should be marked configured after add"
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
        // Default backend should be marked active.
        let active = items.iter().find(|i| i.badge.as_deref() == Some("active"));
        assert!(
            active.is_some(),
            "exactly one memory backend should be active (the default)"
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
    fn channels_picker_marks_configured_after_init_defaults() {
        let mut cfg = empty_cfg();
        cfg.init_defaults(Some("channels.matrix"));
        let items = schema_walk_picker(&cfg, "channels");
        let matrix = items.iter().find(|i| i.key == "matrix").unwrap();
        assert_eq!(
            matrix.badge.as_deref(),
            Some("configured"),
            "matrix should be marked configured after init_defaults"
        );
    }

    #[test]
    fn tunnel_picker_includes_synthetic_none() {
        let cfg = empty_cfg();
        let items = schema_walk_picker_with_none(&cfg, "tunnel", "tunnel.provider");
        assert_eq!(
            items[0].key, "none",
            "`none` must be the first entry in the tunnel picker"
        );
        // `none` is the active default for a fresh config.
        assert_eq!(items[0].badge.as_deref(), Some("active"));
    }
}
