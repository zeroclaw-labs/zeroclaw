//! Onboard orchestrator.
//!
//! Thin dispatcher above the `OnboardUi` trait (defined in
//! `zeroclaw-config::traits`). Section-scoped entry points let callers run
//! just one slice (`zeroclaw onboard channels`) or the whole flow.
//!
//! Everything writes through `Config::set_prop` (or its helpers); direct
//! struct-field assignment is off-limits per the DRY contract (#5951).

use anyhow::Result;
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::{Answer, OnboardUi, PropKind, SelectItem};

/// Internal prompt / section navigation signal. `Done` = advance. `Back` =
/// the user pressed Esc; rewind one step. Helpers propagate it up through
/// `prompt_field` → `prompt_fields_under` → section fn → `run_all`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Nav {
    Done,
    Back,
}

/// Skip-gate outcome. `Skip` = section already configured, user chose not
/// to reconfigure. `Enter` = walk the section. `Back` = user pressed Esc
/// at the skip prompt, bounce to the previous section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipNav {
    Skip,
    Enter,
    Back,
}

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
        Section::All => run_all(cfg, ui, flags).await,
        Section::Workspace => {
            let _ = workspace(cfg, ui, flags).await?;
            Ok(())
        }
        Section::Providers => {
            let _ = providers(cfg, ui, flags).await?;
            Ok(())
        }
        Section::Channels => {
            let _ = channels(cfg, ui, flags).await?;
            Ok(())
        }
        Section::Memory => {
            let _ = memory(cfg, ui, flags).await?;
            Ok(())
        }
        Section::Hardware => {
            let _ = hardware(cfg, ui, flags).await?;
            Ok(())
        }
        Section::Tunnel => {
            let _ = tunnel(cfg, ui, flags).await?;
            Ok(())
        }
    }
}

/// Walk every section in order with section-level Back. Each section returns
/// `Nav::Back` when the user pressed Esc at its first prompt; the loop
/// rewinds to the previous section. Back at the first section exits
/// onboarding cleanly (user bails out).
async fn run_all(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    let mut i: usize = 0;
    loop {
        let nav = match i {
            0 => workspace(cfg, ui, flags).await?,
            1 => providers(cfg, ui, flags).await?,
            2 => channels(cfg, ui, flags).await?,
            3 => memory(cfg, ui, flags).await?,
            4 => hardware(cfg, ui, flags).await?,
            5 => tunnel(cfg, ui, flags).await?,
            _ => return Ok(()),
        };
        match nav {
            Nav::Done => i += 1,
            Nav::Back => {
                if i == 0 {
                    return Ok(());
                }
                i -= 1;
            }
        }
    }
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

/// Prompt for a single config field identified by its dotted name. Returns
/// `Nav::Back` when the user pressed Esc at the prompt; `Nav::Done` on any
/// other outcome (including "kept current value").
async fn prompt_field(cfg: &mut Config, ui: &mut dyn OnboardUi, name: &str) -> Result<Nav> {
    let field = cfg
        .prop_fields()
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown config field: {name}"))?;

    let short = name.rsplit('.').next().unwrap_or(name);
    if !field.description.is_empty() {
        ui.note(field.description);
    }
    let prompt = short;
    let current = field.display_value;
    let is_set = !current.is_empty() && current != "<unset>";

    if field.is_secret {
        match ui.secret(prompt, is_set).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(Some(value)) => persist(cfg, name, &value).await?,
            Answer::Value(None) => {}
        }
        return Ok(Nav::Done);
    }

    match field.kind {
        PropKind::Bool => {
            let cur = current.parse::<bool>().unwrap_or(false);
            match ui.confirm(prompt, cur).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) if new != cur => persist(cfg, name, &new.to_string()).await?,
                Answer::Value(_) => {}
            }
        }
        PropKind::String | PropKind::Integer | PropKind::Float => {
            let default = if is_set { Some(current.as_str()) } else { None };
            match ui.string(prompt, default).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    if (is_set || !new.is_empty()) && new != current {
                        persist(cfg, name, &new).await?;
                    }
                }
            }
        }
        PropKind::Enum => {
            let variants = field.enum_variants.map(|get| get()).unwrap_or_default();
            if variants.is_empty() {
                ui.warn(&format!("skipping {name}: no enum variants exposed"));
                return Ok(Nav::Done);
            }
            let items: Vec<SelectItem> = variants.iter().map(SelectItem::new).collect();
            let current_idx = variants.iter().position(|v| v == &current);
            match ui.select(prompt, &items, current_idx).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(idx) => {
                    let new = &variants[idx];
                    if new != &current {
                        persist(cfg, name, new).await?;
                    }
                }
            }
        }
    }
    Ok(Nav::Done)
}

/// Iterate every field under `prefix` in `prop_fields()` and prompt for each.
/// `excludes` lists leaf field names to skip. Rewinds the iteration on
/// `Nav::Back`; if the user rewinds past the first prompt, propagates `Back`
/// to the caller so the containing section can decide what to do.
async fn prompt_fields_under(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    prefix: &str,
    excludes: &[&str],
) -> Result<Nav> {
    let names: Vec<String> = cfg
        .prop_fields()
        .into_iter()
        .filter_map(|f| {
            let suffix = f.name.strip_prefix(prefix)?.strip_prefix('.')?;
            if suffix.contains('.') || excludes.contains(&suffix) {
                return None;
            }
            Some(f.name.to_string())
        })
        .collect();
    let mut i: usize = 0;
    while i < names.len() {
        match prompt_field(cfg, ui, &names[i]).await? {
            Nav::Done => i += 1,
            Nav::Back => {
                if i == 0 {
                    return Ok(Nav::Back);
                }
                i -= 1;
            }
        }
    }
    Ok(Nav::Done)
}

/// Section-level skip gate. A section is "already configured" when EITHER
/// (a) it has a marker in `onboard_state.completed_sections` (user finished
/// the flow once), OR (b) the caller supplies a section-specific
/// has-meaningful-config signal (e.g. workspace.enabled == true, providers
/// has a fallback + api-key set). `--force` bypasses unconditionally.
async fn skip_if_configured(
    cfg: &Config,
    ui: &mut dyn OnboardUi,
    flags: &Flags,
    section_key: &str,
    label: &str,
    has_signal: bool,
) -> Result<SkipNav> {
    if flags.force {
        return Ok(SkipNav::Enter);
    }
    let seen = cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|s| s == section_key);
    if !seen && !has_signal {
        return Ok(SkipNav::Enter);
    }
    match ui
        .confirm(
            &format!("{label} is already configured. Reconfigure?"),
            false,
        )
        .await?
    {
        Answer::Back => Ok(SkipNav::Back),
        Answer::Value(true) => Ok(SkipNav::Enter),
        Answer::Value(false) => Ok(SkipNav::Skip),
    }
}

/// Per-section meaningful-config detector used as the secondary skip-gate
/// signal alongside the completed_sections marker. Returns true when the
/// section has values that can only come from user action (i.e. diverged
/// from `Config::default()`'s idle state).
fn section_has_signal(cfg: &Config, section_key: &str) -> bool {
    match section_key {
        "workspace" => cfg.workspace.enabled,
        "providers" => cfg.providers.fallback.is_some() && !cfg.providers.models.is_empty(),
        "channels" => cfg
            .prop_fields()
            .iter()
            .any(|f| f.name.starts_with("channels.")),
        "hardware" => cfg.hardware.enabled,
        // Memory's default backend is "sqlite" and Tunnel's is "none" — both
        // are valid user choices indistinguishable from untouched defaults.
        // Marker-only for these two.
        _ => false,
    }
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
// Each section returns `Nav::Back` when the user hits Esc at the very first
// prompt. Back from a later prompt within the section rewinds locally (via
// prompt_fields_under / per-section loop), never propagates to the parent.

async fn workspace(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Workspace");
    ui.status(&format!(
        "Workspace directory: {}",
        cfg.workspace_dir.display()
    ));
    match skip_if_configured(
        cfg,
        ui,
        flags,
        "workspace",
        "Workspace",
        section_has_signal(cfg, "workspace"),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }

    loop {
        match prompt_field(cfg, ui, "workspace.enabled").await? {
            Nav::Back => return Ok(Nav::Back),
            Nav::Done => {}
        }
        if cfg.workspace.enabled {
            match prompt_fields_under(cfg, ui, "workspace", &["enabled"]).await? {
                Nav::Back => continue,
                Nav::Done => break,
            }
        } else {
            break;
        }
    }

    mark_completed(cfg, "workspace").await?;
    Ok(Nav::Done)
}

async fn providers(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Providers");
    if flags.provider.is_none() && flags.api_key.is_none() && flags.model.is_none() {
        match skip_if_configured(
            cfg,
            ui,
            flags,
            "providers",
            "Providers",
            section_has_signal(cfg, "providers"),
        )
        .await?
        {
            SkipNav::Skip => return Ok(Nav::Done),
            SkipNav::Back => return Ok(Nav::Back),
            SkipNav::Enter => {}
        }
    }
    // Surface both auth paths up front so users with an existing key go
    // straight to the api_key prompt, and users on OAuth-only providers
    // (Codex, Claude Code, etc.) know to use the separate login flow.
    ui.note(
        "Paste an API key (e.g. `sk-ant-…` for Anthropic, `sk-…` for OpenAI) \
         when prompted. For OAuth-based providers run: \
         zeroclaw auth login --provider <name>",
    );

    // Menu is driven by zeroclaw_providers::list_providers() — single source
    // of truth for canonical names, display names, aliases.
    let entries = zeroclaw_providers::list_providers();

    loop {
        let current_fallback = cfg.providers.fallback.clone().unwrap_or_default();

        let picked = match &flags.provider {
            Some(forced) => forced.clone(),
            None => {
                let current_idx = entries.iter().position(|p| p.name == current_fallback);
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
                match ui.select("Provider", &options, current_idx).await? {
                    Answer::Back => return Ok(Nav::Back),
                    Answer::Value(idx) => entries[idx].name.to_string(),
                }
            }
        };

        if picked != current_fallback {
            persist(cfg, "providers.fallback", &picked).await?;
        }

        // Seed the HashMap entry so prop_fields enumerates its fields.
        cfg.providers.models.entry(picked.clone()).or_default();
        cfg.save().await?;

        let display_name = entries
            .iter()
            .find(|p| p.name == picked)
            .map(|p| p.display_name)
            .unwrap_or(picked.as_str());
        ui.heading(2, display_name);

        // Apply CLI-flag overrides up front, then skip those names in the
        // interactive pass so the user isn't re-prompted for what they already
        // passed on the command line.
        let prefix = format!("providers.models.{picked}");
        let api_key_path = format!("{prefix}.api-key");
        let excludes: &[&str] = &["model", "api-key"];
        if let Some(api_key) = &flags.api_key {
            persist(cfg, &api_key_path, api_key).await?;
        }
        if let Some(model) = &flags.model {
            persist(cfg, &format!("{prefix}.model"), model).await?;
        }

        // Authentication phase is prompted explicitly so the user sees a
        // clear "API key" step, not a generic `api-key (stored, replace?)`
        // lost among other fields. The heading(2) also overrides the
        // provider subsection so the panel reads "Providers › Authentication".
        if flags.api_key.is_none() {
            ui.heading(2, &format!("{display_name} › Authentication"));
            ui.note(
                "Paste an API key from the provider's dashboard. Enter to keep \
                 the stored key, or `y` at the `replace?` prompt to rotate it. \
                 For OAuth-only providers (Codex, Claude Code), use \
                 `zeroclaw auth login --provider <name>` instead.",
            );
            match prompt_field(cfg, ui, &api_key_path).await? {
                Nav::Back => {
                    if flags.provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    continue;
                }
                Nav::Done => {}
            }
            ui.heading(2, display_name);
        }

        // Remaining provider-specific fields (base-url, region, etc.) —
        // skipped for `model` (handled by prompt_model) and `api-key` (above).
        match prompt_fields_under(cfg, ui, &prefix, excludes).await? {
            Nav::Back => {
                if flags.provider.is_some() {
                    return Ok(Nav::Back);
                }
                continue;
            }
            Nav::Done => {}
        }

        if flags.model.is_none() {
            ui.heading(2, &format!("{display_name} › Model"));
            match prompt_model(cfg, ui, &picked).await? {
                Nav::Back => {
                    if flags.provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    continue;
                }
                Nav::Done => {}
            }
        }
        break;
    }

    mark_completed(cfg, "providers").await?;
    Ok(Nav::Done)
}

/// Per-provider example model-id used in the manual-entry fallback prompt.
/// Small colocated table (per ADR #5951 — consolidating into `ProviderInfo`
/// is a separate follow-up). Providers not listed get a generic example.
fn model_id_hint(provider: &str) -> &'static str {
    match provider {
        "anthropic" => "claude-opus-4-6",
        "openai" => "gpt-4o",
        "openrouter" => "anthropic/claude-opus-4.6",
        "gemini" | "google" => "gemini-2.0-flash",
        "bedrock" => "anthropic.claude-opus-4-v1:0",
        "azure" => "gpt-4o",
        "ollama" => "llama3.2",
        "mistral" => "mistral-large-latest",
        "groq" => "llama-3.3-70b-versatile",
        "deepseek" => "deepseek-chat",
        "xai" | "grok" => "grok-beta",
        _ => "provider/model-id",
    }
}

/// Prompt for the model field using the provider's live model catalog.
///
/// Calls `Provider::list_models()` (no auth — see `zeroclaw-providers`
/// models_dev + native public endpoints). Falls back to a manual string
/// input when the provider doesn't expose a no-auth list or the fetch fails.
async fn prompt_model(cfg: &mut Config, ui: &mut dyn OnboardUi, provider: &str) -> Result<Nav> {
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
            match ui.select("Model", &items, current_idx).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(idx) => models[idx].clone(),
            }
        }
        None => {
            // Live fetch failed / provider doesn't expose no-auth listing.
            // Give the user a provider-flavored nudge so they don't have to
            // guess the model-id format.
            ui.note(&format!(
                "Couldn't reach the provider's model catalog. Type an id manually — e.g. `{}`.",
                model_id_hint(provider)
            ));
            let default = if is_set { Some(current.as_str()) } else { None };
            match ui.string("Model id", default).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(v) => v,
            }
        }
    };

    if new_value != current && !new_value.is_empty() {
        persist(cfg, &model_path, &new_value).await?;
    }
    Ok(Nav::Done)
}

async fn channels(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Channels");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        "channels",
        "Channels",
        section_has_signal(cfg, "channels"),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }
    loop {
        // Master list of all channels that exist in the schema. Probe on a
        // clone: init_defaults(Some("channels")) forces every Option<T>
        // subsection to Some(default), then prop_fields reveals the full
        // set. Feature-gated channels drop out automatically.
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

        let idx = match ui.select("Channel", &options, Some(done_idx)).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) => i,
        };
        if idx == done_idx {
            break;
        }

        let picked = &all_channels[idx];
        let prefix = format!("channels.{picked}");
        cfg.init_defaults(Some(&prefix));
        cfg.save().await?;
        ui.heading(2, picked);
        // Back inside a channel's subfields bounces to the channel list
        // (not to the previous section) — user is still inside Channels.
        let _ = prompt_fields_under(cfg, ui, &prefix, &[]).await?;
    }
    mark_completed(cfg, "channels").await?;
    Ok(Nav::Done)
}

async fn memory(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Memory");
    if flags.memory.is_none() {
        match skip_if_configured(
            cfg,
            ui,
            flags,
            "memory",
            "Memory",
            section_has_signal(cfg, "memory"),
        )
        .await?
        {
            SkipNav::Skip => return Ok(Nav::Done),
            SkipNav::Back => return Ok(Nav::Back),
            SkipNav::Enter => {}
        }
    }
    let backends = zeroclaw_memory::selectable_memory_backends();
    let current_backend = cfg.memory.backend.clone();
    let new_backend = match &flags.memory {
        Some(forced) => forced.clone(),
        None => {
            let options: Vec<SelectItem> =
                backends.iter().map(|b| SelectItem::new(b.label)).collect();
            let current_idx = backends.iter().position(|b| b.key == current_backend);
            match ui.select("Memory backend", &options, current_idx).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(idx) => backends[idx].key.to_string(),
            }
        }
    };
    if new_backend != current_backend {
        persist(cfg, "memory.backend", &new_backend).await?;
    }

    // Back on auto-save bounces to the backend picker (consumed).
    let _ = prompt_field(cfg, ui, "memory.auto-save").await?;
    mark_completed(cfg, "memory").await?;
    Ok(Nav::Done)
}

async fn hardware(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Hardware");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        "hardware",
        "Hardware",
        section_has_signal(cfg, "hardware"),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }

    loop {
        match prompt_field(cfg, ui, "hardware.enabled").await? {
            Nav::Back => return Ok(Nav::Back),
            Nav::Done => {}
        }
        if cfg.hardware.enabled {
            match prompt_fields_under(cfg, ui, "hardware", &["enabled"]).await? {
                Nav::Back => continue,
                Nav::Done => break,
            }
        } else {
            break;
        }
    }
    mark_completed(cfg, "hardware").await?;
    Ok(Nav::Done)
}

async fn tunnel(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Tunnel");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        "tunnel",
        "Tunnel",
        section_has_signal(cfg, "tunnel"),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }

    loop {
        // Provider list derived from the schema: each `tunnel.<name>.*` field
        // in prop_fields() names a real provider. "none" is always valid and
        // has no sub-config, so it's prepended.
        let mut provider_names: Vec<String> = cfg
            .prop_fields()
            .iter()
            .filter_map(|f| f.name.strip_prefix("tunnel."))
            .filter_map(|suffix| suffix.split_once('.').map(|(head, _)| head.to_string()))
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        provider_names.insert(0, "none".to_string());

        let options: Vec<SelectItem> = provider_names.iter().map(SelectItem::new).collect();
        let current_provider = cfg.tunnel.provider.clone();
        let current_idx = provider_names.iter().position(|p| p == &current_provider);
        let idx = match ui
            .select("Public tunnel provider", &options, current_idx)
            .await?
        {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) => i,
        };
        let new_provider = provider_names[idx].clone();

        if new_provider != current_provider {
            persist(cfg, "tunnel.provider", &new_provider).await?;
        }

        if new_provider == "none" {
            break;
        }

        let prefix = format!("tunnel.{new_provider}");
        cfg.init_defaults(Some(&prefix));
        cfg.save().await?;
        ui.heading(2, &new_provider);
        match prompt_fields_under(cfg, ui, &prefix, &[]).await? {
            Nav::Back => continue,
            Nav::Done => break,
        }
    }
    mark_completed(cfg, "tunnel").await?;
    Ok(Nav::Done)
}
