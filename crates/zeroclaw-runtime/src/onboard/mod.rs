//! Onboard orchestrator.
//!
//! Thin dispatcher above the `OnboardUi` trait (defined in
//! `zeroclaw-config::traits`). Section-scoped entry points let callers run
//! just one slice (`zeroclaw onboard channels`) or the whole flow.
//!
//! Sections are stubs in this commit. Each fills in as it's implemented.
//! Everything writes through `Config::set_prop` (or its helpers); direct
//! struct-field assignment is off-limits per the DRY contract (#5951).

use anyhow::Result;
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::{OnboardUi, PropKind, SelectItem};

pub mod ui;

/// Which slice of onboarding to run. `All` runs every section in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    All,
    Workspace,
    Providers,
    Channels,
    Memory,
    Hardware,
    Tunnel,
}

/// Runtime knobs sourced from CLI flags. `--quick`/`--tui` select the UI
/// backend at the binary edge and don't appear here — the orchestrator only
/// cares about per-section behavior.
#[derive(Debug, Default, Clone)]
pub struct Flags {
    /// Skip "keep existing value?" confirmations; always re-prompt.
    pub force: bool,
    /// Back up the current config dir and start from `Config::default()`.
    pub reinit: bool,
    pub api_key: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub memory: Option<String>,
}

/// Top-level onboard dispatcher.
pub async fn run(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    section: Section,
    flags: &Flags,
) -> Result<()> {
    match section {
        Section::All => {
            workspace(cfg, ui, flags).await?;
            providers(cfg, ui, flags).await?;
            channels(cfg, ui, flags).await?;
            memory(cfg, ui, flags).await?;
            hardware(cfg, ui, flags).await?;
            tunnel(cfg, ui, flags).await?;
        }
        Section::Workspace => workspace(cfg, ui, flags).await?,
        Section::Providers => providers(cfg, ui, flags).await?,
        Section::Channels => channels(cfg, ui, flags).await?,
        Section::Memory => memory(cfg, ui, flags).await?,
        Section::Hardware => hardware(cfg, ui, flags).await?,
        Section::Tunnel => tunnel(cfg, ui, flags).await?,
    }
    Ok(())
}

// ── Field-driven helpers ─────────────────────────────────────────────────

/// Prompt for a single config field identified by its dotted name.
///
/// Reads the field's metadata from `prop_fields()` — type, current value,
/// secret flag, enum variants — and dispatches to the right `OnboardUi`
/// method. Writes via `set_prop` only when the value actually changes.
/// Adding a new field to the schema makes it promptable via this helper
/// automatically; no parallel type-dispatch logic to maintain here.
async fn prompt_field(cfg: &mut Config, ui: &mut dyn OnboardUi, name: &str) -> Result<()> {
    let field = cfg
        .prop_fields()
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown config field: {name}"))?;

    let prompt = name.rsplit('.').next().unwrap_or(name);
    let current = field.display_value;
    let is_set = !current.is_empty() && current != "<unset>";

    if field.is_secret {
        if let Some(value) = ui.secret(prompt, is_set).await? {
            cfg.set_prop(name, &value)?;
        }
        return Ok(());
    }

    match field.kind {
        PropKind::Bool => {
            let cur = current.parse::<bool>().unwrap_or(false);
            let new = ui.confirm(prompt, cur).await?;
            if new != cur {
                cfg.set_prop(name, &new.to_string())?;
            }
        }
        PropKind::String | PropKind::Integer | PropKind::Float => {
            let default = if is_set { Some(current.as_str()) } else { None };
            let new = ui.string(prompt, default).await?;
            // Empty input on an unset Option field = leave it unset.
            // Empty input on a set field = would be a clear; set_prop with "" will
            // remove the key (serde_set_prop handles the Option case).
            if new != current && !(new.is_empty() && !is_set) {
                cfg.set_prop(name, &new)?;
            }
        }
        PropKind::Enum => {
            let variants = field.enum_variants.map(|get| get()).unwrap_or_default();
            if variants.is_empty() {
                ui.warn(&format!("skipping {name}: no enum variants exposed"));
                return Ok(());
            }
            let items: Vec<SelectItem> = variants.iter().map(SelectItem::new).collect();
            let current_idx = variants.iter().position(|v| v == &current);
            let idx = ui.select(prompt, &items, current_idx).await?;
            let new = &variants[idx];
            if new != &current {
                cfg.set_prop(name, new)?;
            }
        }
    }
    Ok(())
}

/// Iterate every field under `prefix` in `prop_fields()` and prompt for each.
/// `excludes` lists leaf field names that should be skipped (useful when a
/// section has already handled one field specially — e.g., memory.backend via
/// the memory-backend registry — and just wants the rest prompted generically).
async fn prompt_fields_under(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    prefix: &str,
    excludes: &[&str],
) -> Result<()> {
    let names: Vec<String> = cfg
        .prop_fields()
        .into_iter()
        .filter_map(|f| {
            let suffix = f.name.strip_prefix(prefix)?.strip_prefix('.')?;
            // Skip nested sub-sections (anything with another `.` in it) and
            // anything the caller asked to exclude.
            if suffix.contains('.') || excludes.contains(&suffix) {
                return None;
            }
            Some(f.name.to_string())
        })
        .collect();
    for name in names {
        prompt_field(cfg, ui, &name).await?;
    }
    Ok(())
}

// ── Sections ─────────────────────────────────────────────────────────────
// Each section picks the specialized pre-work (registry-driven choices,
// flag overrides) and then defers to `prompt_fields_under` for the rest.

async fn workspace(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    ui.status(&format!(
        "Workspace directory: {}",
        cfg.workspace_dir.display()
    ));
    prompt_field(cfg, ui, "workspace.enabled").await?;
    if cfg.workspace.enabled {
        prompt_fields_under(cfg, ui, "workspace", &["enabled"]).await?;
    }
    Ok(())
}

async fn providers(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    // Menu is driven by zeroclaw_providers::list_providers() — single source
    // of truth for canonical names, display names, aliases. Badges show which
    // provider is the current fallback and which already have a stored config.
    let entries = zeroclaw_providers::list_providers();
    let current_fallback = cfg.providers.fallback.clone().unwrap_or_default();
    let current_idx = entries.iter().position(|p| p.name == current_fallback);

    let picked = if let Some(forced) = &flags.provider {
        forced.clone()
    } else {
        let options: Vec<SelectItem> = entries
            .iter()
            .map(|p| {
                let configured = cfg.providers.models.contains_key(p.name);
                let is_active = p.name == current_fallback;
                let badge = match (is_active, configured) {
                    (true, _) => Some("[active]".into()),
                    (_, true) => Some("[configured]".into()),
                    _ => None,
                };
                SelectItem {
                    label: p.display_name.to_string(),
                    badge,
                }
            })
            .collect();
        let idx = ui.select("Provider", &options, current_idx).await?;
        entries[idx].name.to_string()
    };

    if picked != current_fallback {
        cfg.set_prop("providers.fallback", &picked)?;
    }

    // Seed the HashMap entry so prop_fields enumerates its fields. Default
    // ModelProviderConfig is empty; set_prop fills it as we prompt.
    cfg.providers
        .models
        .entry(picked.clone())
        .or_insert_with(Default::default);

    // Apply CLI-flag overrides up front, then skip those names in the
    // interactive pass so the user isn't re-prompted for what they already
    // passed on the command line.
    let prefix = format!("providers.models.{picked}");
    let mut excludes: Vec<&str> = Vec::new();
    if let Some(api_key) = &flags.api_key {
        cfg.set_prop(&format!("{prefix}.api-key"), api_key)?;
        excludes.push("api-key");
    }
    if let Some(model) = &flags.model {
        cfg.set_prop(&format!("{prefix}.model"), model)?;
        excludes.push("model");
    }

    prompt_fields_under(cfg, ui, &prefix, &excludes).await?;
    Ok(())
}

async fn channels(_cfg: &mut Config, _ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    Ok(())
}

async fn memory(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    // Backend: registry-driven select (key + label both come from
    // zeroclaw-memory's selectable_memory_backends()). --memory CLI flag
    // bypasses the prompt.
    let backends = zeroclaw_memory::selectable_memory_backends();
    let current_backend = cfg.memory.backend.clone();
    let new_backend = if let Some(forced) = &flags.memory {
        forced.clone()
    } else {
        let options: Vec<SelectItem> = backends.iter().map(|b| SelectItem::new(b.label)).collect();
        let current_idx = backends.iter().position(|b| b.key == current_backend);
        let idx = ui.select("Memory backend", &options, current_idx).await?;
        backends[idx].key.to_string()
    };
    if new_backend != current_backend {
        cfg.set_prop("memory.backend", &new_backend)?;
    }

    prompt_field(cfg, ui, "memory.auto-save").await
}

async fn hardware(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    prompt_field(cfg, ui, "hardware.enabled").await?;
    if cfg.hardware.enabled {
        prompt_fields_under(cfg, ui, "hardware", &["enabled"]).await?;
    }
    Ok(())
}

async fn tunnel(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<()> {
    // Provider list is derived from the schema: each `tunnel.<name>.*` field
    // in prop_fields() names a real provider. "none" is always valid and has
    // no sub-config, so it's prepended. Adding a new TunnelConfig subsection
    // surfaces here automatically — no parallel list to drift.
    let mut providers: Vec<String> = cfg
        .prop_fields()
        .iter()
        .filter_map(|f| f.name.strip_prefix("tunnel."))
        .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    providers.insert(0, "none".to_string());

    let options: Vec<SelectItem> = providers.iter().map(SelectItem::new).collect();
    let current_provider = cfg.tunnel.provider.clone();
    let current_idx = providers.iter().position(|p| p == &current_provider);
    let idx = ui
        .select("Public tunnel provider", &options, current_idx)
        .await?;
    let new_provider = providers[idx].clone();

    if new_provider != current_provider {
        cfg.set_prop("tunnel.provider", &new_provider)?;
    }

    if new_provider != "none" {
        // Materialize the Option<T> sub-config so its fields become enumerable,
        // then prompt every field generically.
        let prefix = format!("tunnel.{new_provider}");
        cfg.init_defaults(Some(&prefix));
        prompt_fields_under(cfg, ui, &prefix, &[]).await?;
    }
    Ok(())
}
