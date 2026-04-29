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
}

#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
pub struct SectionsResponse {
    pub sections: Vec<SectionInfo>,
}

/// `GET /api/onboard/sections` — list onboarding sections in display order.
/// Mirrors the TUI's section ordering. Single source of truth for what
/// sections exist; the dashboard renders one entry per item.
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

    let sections = vec![
        section_info(
            "workspace",
            "Workspace",
            workspace_help(),
            false,
            &completed,
        ),
        section_info("providers", "Providers", providers_help(), true, &completed),
        section_info("channels", "Channels", channels_help(), true, &completed),
        section_info("memory", "Memory", memory_help(), true, &completed),
        section_info("hardware", "Hardware", hardware_help(), false, &completed),
        section_info("tunnel", "Tunnel", tunnel_help(), true, &completed),
    ];
    axum::Json(SectionsResponse { sections }).into_response()
}

fn section_info(
    key: &str,
    label: &str,
    help: &str,
    has_picker: bool,
    completed: &std::collections::HashSet<String>,
) -> SectionInfo {
    SectionInfo {
        key: key.to_string(),
        label: label.to_string(),
        help: help.to_string(),
        has_picker,
        completed: completed.contains(key),
    }
}

// Section help text — lifted verbatim from the TUI's `ui.note(...)` calls so
// the CLI and web wizard surface identical copy. If the TUI text changes,
// these should follow. Could be extracted to a shared module of static
// strings if drift becomes a concern.
fn workspace_help() -> &'static str {
    "Where ZeroClaw stores its config and runtime data. Defaults work for most setups."
}
fn providers_help() -> &'static str {
    "Paste an API key (e.g. `sk-ant-...` for Anthropic, `sk-...` for OpenAI) when prompted. \
     For OAuth-based providers run: zeroclaw auth login --provider <name>"
}
fn channels_help() -> &'static str {
    "Pick which chat platforms ZeroClaw should listen on. You can configure multiple."
}
fn memory_help() -> &'static str {
    "Persistent memory backend. SQLite is recommended; pick `none` to disable."
}
fn hardware_help() -> &'static str {
    "Optional: hardware peripherals (Arduino, STM32, GPIO, etc.). Skip if you don't need them."
}
fn tunnel_help() -> &'static str {
    "Optional: expose your gateway over the public internet via Cloudflare or ngrok. \
     Pick `none` to keep it localhost-only."
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
        "providers" => (providers_picker(&cfg), providers_help().to_string()),
        "memory" => (memory_picker(&cfg), memory_help().to_string()),
        "channels" => (
            schema_walk_picker(&cfg, "channels"),
            channels_help().to_string(),
        ),
        "tunnel" => (
            schema_walk_picker_with_none(&cfg, "tunnel", "tunnel.provider"),
            tunnel_help().to_string(),
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
        .map(|name| PickerItem {
            key: name.clone(),
            label: name.clone(),
            description: None,
            badge: if configured.contains(&name) {
                Some("configured".to_string())
            } else {
                None
            },
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
