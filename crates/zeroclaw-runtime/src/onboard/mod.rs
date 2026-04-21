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

/// Write a single property and immediately persist the whole config. This is
/// the ONE path every section takes to mutate cfg, so users who Ctrl+C
/// mid-flow find their prior answers already saved on disk — re-running
/// `zeroclaw onboard` picks up where they left off.
async fn persist(cfg: &mut Config, path: &str, value: &str) -> Result<()> {
    cfg.set_prop(path, value)?;
    cfg.save().await?;
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

    // Surface the field's `///` doc comment as context ABOVE the prompt
    // (via ui.note) rather than cramming it into the prompt label. The
    // prompt itself shows just the short name — clean one-liner, with the
    // explanation rendered in the log/status area where users actually read
    // prose.
    let short = name.rsplit('.').next().unwrap_or(name);
    if !field.description.is_empty() {
        ui.note(field.description);
    }
    let prompt = short;
    let current = field.display_value;
    let is_set = !current.is_empty() && current != "<unset>";

    if field.is_secret {
        if let Some(value) = ui.secret(prompt, is_set).await? {
            persist(cfg, name, &value).await?;
        }
        return Ok(());
    }

    match field.kind {
        PropKind::Bool => {
            let cur = current.parse::<bool>().unwrap_or(false);
            let new = ui.confirm(prompt, cur).await?;
            if new != cur {
                persist(cfg, name, &new.to_string()).await?;
            }
        }
        PropKind::String | PropKind::Integer | PropKind::Float => {
            let default = if is_set { Some(current.as_str()) } else { None };
            let new = ui.string(prompt, default).await?;
            // Empty input on an unset Option field = leave it unset.
            // Empty input on a set field = would be a clear; set_prop with "" will
            // remove the key (serde_set_prop handles the Option case).
            if (is_set || !new.is_empty()) && new != current {
                persist(cfg, name, &new).await?;
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
                persist(cfg, name, new).await?;
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

/// Section-level skip gate. The signal is `onboard_state.completed_sections`
/// — sections that have actually been walked through before. `--force`
/// bypasses (always reconfigure), as does any per-section CLI override flag
/// the caller deems relevant.
async fn skip_if_configured(
    cfg: &Config,
    ui: &mut dyn OnboardUi,
    flags: &Flags,
    section_key: &str,
    label: &str,
) -> Result<bool> {
    if flags.force {
        return Ok(false);
    }
    let seen = cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|s| s == section_key);
    if !seen {
        return Ok(false);
    }
    let reconfigure = ui
        .confirm(
            &format!("{label} is already configured. Reconfigure?"),
            false,
        )
        .await?;
    Ok(!reconfigure)
}

/// Record that a section finished so the next run's skip gate can fire.
async fn mark_completed(cfg: &mut Config, section_key: &str) -> Result<()> {
    if cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|s| s == section_key)
    {
        return Ok(());
    }
    cfg.onboard_state
        .completed_sections
        .push(section_key.to_string());
    cfg.save().await?;
    Ok(())
}

// ── Sections ─────────────────────────────────────────────────────────────
// Each section picks the specialized pre-work (registry-driven choices,
// flag overrides) and then defers to `prompt_fields_under` for the rest.

async fn workspace(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    ui.status(&format!(
        "Workspace directory: {}",
        cfg.workspace_dir.display()
    ));
    if skip_if_configured(cfg, ui, flags, "workspace", "Workspace").await? {
        return Ok(());
    }
    prompt_field(cfg, ui, "workspace.enabled").await?;
    if cfg.workspace.enabled {
        prompt_fields_under(cfg, ui, "workspace", &["enabled"]).await?;
    }
    mark_completed(cfg, "workspace").await
}

async fn providers(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    if flags.provider.is_none()
        && skip_if_configured(cfg, ui, flags, "providers", "Providers").await?
    {
        return Ok(());
    }
    // Surface both auth paths up front so users with an existing key go
    // straight to the api_key prompt, and users on OAuth-only providers
    // (Codex, Claude Code, etc.) know to use the separate login flow.
    // No provider list encoded here — auth login's own provider match is
    // the source of truth for which names it supports.
    ui.note(
        "Paste an API key (e.g. `sk-ant-…` for Anthropic, `sk-…` for OpenAI) \
         when prompted. For OAuth-based providers run: \
         zeroclaw auth login --provider <name>",
    );
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
        persist(cfg, "providers.fallback", &picked).await?;
    }

    // Seed the HashMap entry so prop_fields enumerates its fields. Default
    // ModelProviderConfig is empty; set_prop fills it as we prompt.
    cfg.providers.models.entry(picked.clone()).or_default();
    cfg.save().await?;

    // Apply CLI-flag overrides up front, then skip those names in the
    // interactive pass so the user isn't re-prompted for what they already
    // passed on the command line.
    let prefix = format!("providers.models.{picked}");
    let mut excludes: Vec<&str> = vec!["model"]; // handled below via live fetch
    if let Some(api_key) = &flags.api_key {
        persist(cfg, &format!("{prefix}.api-key"), api_key).await?;
        excludes.push("api-key");
    }
    if let Some(model) = &flags.model {
        persist(cfg, &format!("{prefix}.model"), model).await?;
        prompt_fields_under(cfg, ui, &prefix, &excludes).await?;
        return mark_completed(cfg, "providers").await;
    }

    prompt_fields_under(cfg, ui, &prefix, &excludes).await?;
    prompt_model(cfg, ui, &picked).await?;
    mark_completed(cfg, "providers").await
}

/// Prompt for the model field using the provider's live model catalog.
///
/// Calls `Provider::list_models()` (no auth — see `zeroclaw-providers`
/// models_dev + native public endpoints). Falls back to a manual string
/// input when the provider doesn't expose a no-auth list or the fetch fails.
async fn prompt_model(cfg: &mut Config, ui: &mut dyn OnboardUi, provider: &str) -> Result<()> {
    let model_path = format!("providers.models.{provider}.model");
    let current = cfg.get_prop(&model_path).unwrap_or_default();
    let is_set = !current.is_empty() && current != "<unset>";

    let live_models = match zeroclaw_providers::create_provider(provider, None) {
        Ok(handle) => handle.list_models().await.ok(),
        Err(_) => None,
    };

    let new_value = match live_models.filter(|ms| !ms.is_empty()) {
        Some(models) => {
            let items: Vec<SelectItem> = models.iter().map(SelectItem::new).collect();
            let current_idx = models.iter().position(|m| m == &current);
            let idx = ui.select("Model", &items, current_idx).await?;
            models[idx].clone()
        }
        None => {
            let default = if is_set { Some(current.as_str()) } else { None };
            ui.string("Model id", default).await?
        }
    };

    if new_value != current && !new_value.is_empty() {
        persist(cfg, &model_path, &new_value).await?;
    }
    Ok(())
}

async fn channels(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    if skip_if_configured(cfg, ui, flags, "channels", "Channels").await? {
        return Ok(());
    }
    loop {
        // Master list of all channels that exist in the schema. Probe on a
        // clone: init_defaults(Some("channels")) forces every Option<T>
        // subsection to Some(default), then prop_fields reveals the full
        // set. Feature-gated channels (channel-nostr, voice-wake, …) are
        // absent from the compiled struct, so they drop out automatically.
        let all_channels: Vec<String> = {
            let mut probe = cfg.clone();
            probe.init_defaults(Some("channels"));
            probe
                .prop_fields()
                .iter()
                .filter_map(|f| f.name.strip_prefix("channels."))
                .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        // Which of those are already configured in the real cfg?
        let configured: std::collections::BTreeSet<String> = cfg
            .prop_fields()
            .iter()
            .filter_map(|f| f.name.strip_prefix("channels."))
            .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
            .collect();

        let mut options: Vec<SelectItem> = all_channels
            .iter()
            .map(|name| {
                if configured.contains(name) {
                    SelectItem::with_badge(name.clone(), "[configured]")
                } else {
                    SelectItem::new(name.clone())
                }
            })
            .collect();
        let done_idx = options.len();
        options.push(SelectItem::new("Done"));

        let idx = ui.select("Channel", &options, Some(done_idx)).await?;
        if idx == done_idx {
            break;
        }

        let picked = &all_channels[idx];
        let prefix = format!("channels.{picked}");
        cfg.init_defaults(Some(&prefix));
        cfg.save().await?;
        prompt_fields_under(cfg, ui, &prefix, &[]).await?;
    }
    mark_completed(cfg, "channels").await
}

async fn memory(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    if flags.memory.is_none()
        && skip_if_configured(cfg, ui, flags, "memory", "Memory").await?
    {
        return Ok(());
    }
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
        persist(cfg, "memory.backend", &new_backend).await?;
    }

    prompt_field(cfg, ui, "memory.auto-save").await?;
    mark_completed(cfg, "memory").await
}

async fn hardware(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    if skip_if_configured(cfg, ui, flags, "hardware", "Hardware").await? {
        return Ok(());
    }
    prompt_field(cfg, ui, "hardware.enabled").await?;
    if cfg.hardware.enabled {
        prompt_fields_under(cfg, ui, "hardware", &["enabled"]).await?;
    }
    mark_completed(cfg, "hardware").await
}

async fn tunnel(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    ui.note("");
    if skip_if_configured(cfg, ui, flags, "tunnel", "Tunnel").await? {
        return Ok(());
    }
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
        persist(cfg, "tunnel.provider", &new_provider).await?;
    }

    if new_provider != "none" {
        // Materialize the Option<T> sub-config so its fields become enumerable,
        // then prompt every field generically.
        let prefix = format!("tunnel.{new_provider}");
        cfg.init_defaults(Some(&prefix));
        cfg.save().await?;
        prompt_fields_under(cfg, ui, &prefix, &[]).await?;
    }
    mark_completed(cfg, "tunnel").await
}
