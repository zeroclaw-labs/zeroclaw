//! Curated section picker and select logic shared by the gateway's
//! `/api/config/sections/{section}/...` routes and the RPC
//! `config/section-picker` / `config/section-select` methods.
//!
//! Both surfaces call [`section_picker`] and [`apply_section_select`] so the
//! item lists, badges, created flags and refusals are identical; each surface
//! only adds its own auth and commit path.

use zeroclaw_config::api_error::{ConfigApiCode, ConfigApiError};
use zeroclaw_config::schema::Config;
use zeroclaw_config::sections::Section;

use crate::rpc::types::{PickerItem, PickerResponse, SelectItemResponse};

/// Build the picker for `section`, or the refusal a direct-form or unknown
/// section gets.
pub fn section_picker(section: &str, cfg: &Config) -> Result<PickerResponse, ConfigApiError> {
    let Some(section_enum) = Section::from_key(section) else {
        return Err(ConfigApiError::new(
            ConfigApiCode::PathNotFound,
            format!(
                "section `{section}` has no picker; render its fields \
                 via GET /api/config/list?prefix={section}"
            ),
        )
        .with_path(section));
    };
    let help = zeroclaw_config::sections::section_help(section_enum.as_str()).to_string();
    let items = match picker_items_for(section_enum, cfg) {
        PickerDispatch::Items(items) => items,
        PickerDispatch::DirectForm => return Err(direct_form_error(section_enum)),
    };
    Ok(PickerResponse {
        section: section.to_string(),
        items,
        help,
    })
}

fn direct_form_error(section: Section) -> ConfigApiError {
    ConfigApiError::new(
        ConfigApiCode::PathNotFound,
        format!(
            "section `{section}` is a direct-form section with no picker; \
             render fields via GET /api/config/list?prefix={section}"
        ),
    )
    .with_path(section.as_str())
}

/// Normalize the optional alias a select request carries: trimmed, and
/// `default` when absent or blank.
pub fn select_alias(alias: Option<&str>) -> String {
    alias
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

/// The config path a selection writes, known before the selection runs.
/// Callers that authorize per path check this before [`apply_section_select`]
/// because an agent selection scaffolds a workspace on disk as a side effect.
pub fn select_target_path(section: &str, key: &str, alias: &str) -> Result<String, ConfigApiError> {
    let Some(section_enum) = Section::from_key(section) else {
        return Err(unknown_select_section(section));
    };
    Ok(match section_enum {
        Section::ModelProviders
        | Section::TtsProviders
        | Section::TranscriptionProviders
        | Section::Channels
        | Section::Storage => format!("{}.{key}.{alias}", section_enum.as_str()),
        Section::Memory => "memory.backend".to_string(),
        Section::Tunnel => "tunnel.tunnel_provider".to_string(),
        Section::Hardware | Section::Mcp | Section::Skills | Section::QuickstartState => {
            return Err(direct_form_error(section_enum));
        }
        _ => format!("{}.{key}", section_enum.as_str()),
    })
}

fn unknown_select_section(section: &str) -> ConfigApiError {
    ConfigApiError::new(
        ConfigApiCode::PathNotFound,
        format!("no picker semantics defined for section `{section}`"),
    )
    .with_path(section)
}

/// Result of [`apply_section_select`] on a working copy.
pub struct SectionSelectOutcome {
    pub response: SelectItemResponse,
    /// `true` when `working` changed and the caller must persist and swap it.
    pub needs_persist: bool,
}

/// Apply a picker selection to `working`. The caller holds its config write
/// lock, persists `working` when `needs_persist` is set, and returns
/// `response` either way. A no-op memory or tunnel selection whose on-disk
/// state has drifted is refused so the operator does not act on a stale view.
pub async fn apply_section_select(
    working: &mut Config,
    section: &str,
    key: &str,
    alias: &str,
) -> Result<SectionSelectOutcome, ConfigApiError> {
    let Some(section_enum) = Section::from_key(section) else {
        return Err(unknown_select_section(section));
    };

    let (fields_prefix, created) = match section_enum {
        Section::ModelProviders | Section::TtsProviders | Section::TranscriptionProviders => {
            let family = section_enum.as_str();
            let created = working
                .create_map_key(&format!("{family}.{key}"), alias)
                .map_err(|msg| {
                    ConfigApiError::new(
                        ConfigApiCode::PathNotFound,
                        format!("could not select {family} `{key}` alias `{alias}`: {msg}"),
                    )
                    .with_path(format!("{family}.{key}"))
                })?;
            // Per-family typed configs derive their own default endpoint
            // URI via family traits at runtime construction time.
            (format!("{family}.{key}.{alias}"), created)
        }
        Section::Channels => {
            let created = working
                .create_map_key(&format!("channels.{key}"), alias)
                .map_err(|msg| {
                    ConfigApiError::new(
                        ConfigApiCode::PathNotFound,
                        format!("could not select channel `{key}` alias `{alias}`: {msg}"),
                    )
                    .with_path(format!("channels.{key}"))
                })?;
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
        | Section::DecisionModels
        | Section::Cron
        | Section::McpServers
        | Section::McpBundles
        | Section::KnowledgeBundles
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles
        | Section::ModelRoutes
        | Section::EmbeddingRoutes => {
            let section_key = section_enum.as_str();
            let created = match zeroclaw_config::alias_refs::create_map_key_checked(
                working,
                section_key,
                key,
            ) {
                Ok(c) => c,
                Err(zeroclaw_config::alias_refs::CreateError::Reserved(a)) => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!("alias `{a}` is reserved and cannot be created"),
                    )
                    .with_path(format!("{section_key}.{key}")));
                }
                Err(zeroclaw_config::alias_refs::CreateError::Invalid(msg)) => {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::PathNotFound,
                        format!("could not select {section_key} alias `{key}`: {msg}"),
                    )
                    .with_path(section_key));
                }
            };
            // Agents need a per-alias workspace dir on disk so the
            // PersonalityEditor and the runtime have somewhere to read
            // and write IDENTITY.md / SOUL.md / USER.md / etc.
            if created && matches!(section_enum, Section::Agents) {
                apply_first_run_agent_defaults(working, key);
                let workspace_dir = working.agent_workspace_dir(key);
                if let Err(err) = tokio::fs::create_dir_all(&workspace_dir).await {
                    return Err(ConfigApiError::new(
                        ConfigApiCode::ValidationFailed,
                        format!(
                            "created agent `{key}` but failed to scaffold workspace at {}: {err}",
                            workspace_dir.display()
                        ),
                    )
                    .with_path(section_key));
                }
                if let Err(err) = crate::agent::personality::seed_default_personality(
                    working,
                    key,
                    &workspace_dir,
                )
                .await
                {
                    ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": key, "workspace": workspace_dir.display().to_string(), "err": err.to_string()})), "agent workspace scaffolded but personality seed failed (continuing)");
                }
            }
            (format!("{section_key}.{key}"), created)
        }
        Section::Storage => {
            let created = working
                .create_map_key(&format!("storage.{key}"), alias)
                .map_err(|msg| {
                    ConfigApiError::new(
                        ConfigApiCode::PathNotFound,
                        format!("could not select storage `{key}` alias `{alias}`: {msg}"),
                    )
                    .with_path(format!("storage.{key}"))
                })?;
            mark_section_completed(working, "storage");
            (format!("storage.{key}.{alias}"), created)
        }
        Section::Memory => {
            // Set memory.backend to the picked key. Fields_prefix points at
            // `memory` so the form renders the whole memory section
            // (the active backend's specific fields show up there).
            let selection_changed = working.memory.backend != key;
            if selection_changed && let Err(e) = working.set_prop_persistent("memory.backend", key)
            {
                return Err(ConfigApiError::new(
                    ConfigApiCode::ValidationFailed,
                    format!("could not set memory.backend = `{key}`: {e}"),
                )
                .with_path("memory.backend"));
            }
            let completion_changed = mark_section_completed(working, "memory");
            (
                "memory".to_string(),
                selection_changed || completion_changed,
            )
        }
        Section::Tunnel => {
            let selection_changed = working.tunnel.tunnel_provider != key;
            if selection_changed
                && let Err(e) = working.set_prop_persistent("tunnel.tunnel_provider", key)
            {
                return Err(ConfigApiError::new(
                    ConfigApiCode::ValidationFailed,
                    format!("could not set tunnel.tunnel_provider = `{key}`: {e}"),
                )
                .with_path("tunnel.tunnel_provider"));
            }
            let (prefix, defaults_changed) = if key == "none" {
                ("tunnel".to_string(), false)
            } else {
                let p = format!("tunnel.{key}");
                let initialized = working.init_defaults(Some(&p));
                (p, !initialized.is_empty())
            };
            (prefix, selection_changed || defaults_changed)
        }
        Section::Hardware | Section::Mcp | Section::Skills | Section::QuickstartState => {
            return Err(direct_form_error(section_enum));
        }
    };

    if created {
        working.mark_dirty(&fields_prefix);
    }

    if working.dirty_paths.is_empty() {
        let drifted = match section_enum {
            Section::Memory | Section::Tunnel => super::drift::try_compute_drift(working).await?,
            _ => Vec::new(),
        };
        let tunnel_prefix =
            (section_enum == Section::Tunnel && key != "none").then(|| format!("tunnel.{key}."));
        let conflict_paths: Vec<String> = drifted
            .into_iter()
            .filter(|drift| match section_enum {
                Section::Memory => drift.path == "memory.backend",
                Section::Tunnel => {
                    drift.path == "tunnel.tunnel_provider"
                        || tunnel_prefix
                            .as_deref()
                            .is_some_and(|prefix| drift.path.starts_with(prefix))
                }
                _ => false,
            })
            .map(|drift| drift.path)
            .collect();
        if !conflict_paths.is_empty() {
            return Err(ConfigApiError::new(
                ConfigApiCode::ConfigChangedExternally,
                format!(
                    "on-disk config has drifted from in-memory state on {} path(s) required by this selection: {}. Reload the config or GET /api/config/drift to inspect first.",
                    conflict_paths.len(),
                    conflict_paths.join(", "),
                ),
            ));
        }
        return Ok(SectionSelectOutcome {
            response: SelectItemResponse {
                fields_prefix,
                created,
            },
            needs_persist: false,
        });
    }

    Ok(SectionSelectOutcome {
        response: SelectItemResponse {
            fields_prefix,
            created,
        },
        needs_persist: true,
    })
}

pub enum PickerDispatch {
    Items(Vec<PickerItem>),
    DirectForm,
}

/// Per-section picker dispatch. Exhaustive over [`Section`] so adding a
/// variant fails to compile until it gets a routing arm. The DRY
/// version of what the dashboard's per-section view boils down to.
pub fn picker_items_for(
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
        Section::Tunnel => PickerDispatch::Items(tunnel_provider_picker(cfg)),
        Section::Agents => PickerDispatch::Items(agents_picker(cfg)),
        // Storage is two-tier (`storage.<kind>.<alias>`) — same shape
        // and walker as channels and the typed-provider families.
        Section::Storage => PickerDispatch::Items(storage_picker(cfg)),
        // OneTierAliasMap explorer sections: pick a key from the live
        // HashMap. Generic walker covers every section whose schema is
        // `<section>.<alias>` (operator-named keys, no closed kind set).
        Section::PeerGroups
        | Section::DecisionModels
        | Section::Cron
        | Section::McpServers
        | Section::McpBundles
        | Section::KnowledgeBundles
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles
        | Section::ModelRoutes
        | Section::EmbeddingRoutes => {
            PickerDispatch::Items(one_tier_alias_map_picker(cfg, section.as_str()))
        }
        Section::Hardware | Section::Mcp | Section::Skills | Section::QuickstartState => {
            PickerDispatch::DirectForm
        }
    }
}

pub fn providers_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
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

pub fn any_usable_model_provider(cfg: &zeroclaw_config::schema::Config) -> bool {
    cfg.providers
        .models
        .iter_entries()
        .any(|(family, _, base)| {
            model_provider_alias_usable(base, crate::quickstart::model_provider_is_local(family))
        })
}

pub fn provider_type_badge(
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

pub fn model_provider_alias_usable(
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

pub fn storage_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
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

pub fn memory_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
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

pub fn schema_walk_picker(cfg: &zeroclaw_config::schema::Config, section: &str) -> Vec<PickerItem> {
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

pub fn one_tier_alias_map_picker(
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
pub fn agents_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
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

pub fn apply_first_run_agent_defaults(cfg: &mut zeroclaw_config::schema::Config, alias: &str) {
    let model_provider = cfg
        .providers
        .models
        .iter_entries()
        .next()
        .map(|(ty, alias, _)| format!("{ty}.{alias}"));
    let risk_profile = first_alias(cfg.risk_profiles.keys());
    let runtime_profile = first_alias(cfg.runtime_profiles.keys());

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
        agent.risk_profile = risk_profile.into();
    }
    if agent.runtime_profile.trim().is_empty()
        && let Some(runtime_profile) = runtime_profile
    {
        agent.runtime_profile = runtime_profile.into();
    }
}

pub fn mark_section_completed(cfg: &mut zeroclaw_config::schema::Config, section: &str) -> bool {
    if cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|completed| completed == section)
    {
        return false;
    }
    cfg.onboard_state
        .completed_sections
        .push(section.to_string());
    cfg.mark_dirty("onboard_state.completed_sections");
    true
}

fn first_alias<'a>(aliases: impl Iterator<Item = &'a String>) -> Option<String> {
    let mut aliases: Vec<&String> = aliases.collect();
    aliases.sort();
    aliases.first().map(|alias| (*alias).clone())
}

pub fn tunnel_provider_picker(cfg: &zeroclaw_config::schema::Config) -> Vec<PickerItem> {
    // The canonical prop name uses an underscore (`tunnel_provider`); the
    // hyphenated form is unknown to get_prop and silently yields "" (no active
    // provider ever badged).
    let active = cfg.get_prop("tunnel.tunnel_provider").unwrap_or_default();
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
    for entry in cfg.tunnel.nested_option_entries() {
        let badge = if entry.field == active {
            Some("active".to_string())
        } else if entry.present {
            Some("configured".to_string())
        } else {
            None
        };
        items.push(PickerItem {
            key: entry.field.to_string(),
            label: entry.display_name.to_string(),
            description: if entry.description.is_empty() {
                None
            } else {
                Some(entry.description.to_string())
            },
            badge,
        });
    }
    items
}
