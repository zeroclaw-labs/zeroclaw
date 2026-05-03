//! Onboard orchestrator.
//!
//! Thin dispatcher above the `OnboardUi` trait (defined in
//! `zeroclaw-config::traits`). Section-scoped entry points let callers run
//! just one slice (`zeroclaw onboard channels`) or the whole flow.
//!
//! Everything writes through `Config::set_prop` (or its helpers); direct
//! struct-field assignment is off-limits per the DRY contract (#5951).

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::{Answer, OnboardUi, PropKind, SelectItem};

use crate::agent::personality::EDITABLE_PERSONALITY_FILES;
use crate::agent::personality_templates::{TemplateContext, render as render_personality};

const CUSTOM_OPENAI_COMPAT_LABEL: &str = "Custom OpenAI-compatible endpoint";
const OPENAI_COMPAT_MODELS_TIMEOUT: Duration = Duration::from_secs(10);

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

pub mod field_visibility;
pub mod ui;

#[derive(Debug, Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAiModel {
    id: String,
}

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
    Personality,
}

impl Section {
    /// Stable string name used in TOML paths (`providers.fallback`, etc.) and
    /// surfaced over the HTTP CRUD API for grouping prop lists by section.
    /// `All` has no path representation; returns `None`.
    pub fn as_path_prefix(self) -> Option<&'static str> {
        match self {
            Self::All => None,
            Self::Workspace => Some("workspace"),
            Self::Providers => Some("providers"),
            Self::Channels => Some("channels"),
            Self::Memory => Some("memory"),
            Self::Hardware => Some("hardware"),
            Self::Tunnel => Some("tunnel"),
            Self::Personality => Some("personality"),
        }
    }

    /// Map a dotted property path back to its onboarding section by looking
    /// at the path's first segment. Returns `None` for paths that don't fall
    /// into any wizard section (e.g. `onboard_state.completed_sections`).
    ///
    /// This is the substitute for a `#[onboard_section]` schema attribute —
    /// the section is implicit in the path, derived once from this table
    /// rather than duplicated on every field.
    pub fn from_path(path: &str) -> Option<Self> {
        let prefix = path.split('.').next()?;
        match prefix {
            "workspace" => Some(Self::Workspace),
            "providers" => Some(Self::Providers),
            "channels" => Some(Self::Channels),
            "memory" => Some(Self::Memory),
            "hardware" => Some(Self::Hardware),
            "tunnel" => Some(Self::Tunnel),
            "personality" => Some(Self::Personality),
            _ => None,
        }
    }
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
        Section::Personality => {
            let _ = personality(cfg, ui, flags).await?;
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
            // Personality lives at the end so the user has answered the
            // structural questions (workspace, providers, memory, …)
            // before authoring the markdown files that reference them.
            6 => personality(cfg, ui, flags).await?,
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

/// Per-field default override. When a section knows a sensible default
/// that lives outside the config (e.g. `AnthropicProvider::default_temperature()`),
/// it builds a list of these and passes them to `prompt_fields_under`.
/// The prompt surfaces the default — shown in the label as
/// `"timeout-secs (default: 120)"` and prefilled into the input so the
/// user just hits Enter to accept — only when the field is unset in cfg.
#[derive(Debug, Clone)]
pub struct FieldDefault {
    pub path: String,
    pub display: String,
}

fn find_default<'a>(defaults: &'a [FieldDefault], path: &str) -> Option<&'a str> {
    defaults
        .iter()
        .find(|d| d.path == path)
        .map(|d| d.display.as_str())
}

/// True when `input` parses as the same `Vec<String>` form `config.toml`
/// emits. Lets the StringArray prompt accept the bracketed display form
/// bidirectionally.
fn parses_as_string_array(input: &str) -> bool {
    toml::from_str::<std::collections::HashMap<String, Vec<String>>>(&format!("v = {input}"))
        .is_ok()
}

/// Prompt for a single config field identified by its dotted name. Returns
/// `Nav::Back` when the user pressed Esc at the prompt; `Nav::Done` on any
/// other outcome (including "kept current value"). `default` is the
/// section-supplied fallback for unset fields — surfaced in the label and
/// prefilled into the input.
async fn prompt_field(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    name: &str,
    default: Option<&str>,
) -> Result<Nav> {
    let field = cfg
        .prop_fields()
        .into_iter()
        .find(|f| f.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown config field: {name}"))?;

    let short = name.rsplit('.').next().unwrap_or(name);
    let current = field.display_value;
    // For bools, `display_value` is always `"true"` or `"false"` — never
    // empty, never `"<unset>"` — so a naive is-set check can't tell an
    // explicit user choice apart from the struct-level default. Treat
    // bools as unset here: the [Yes]/[No] toggle already surfaces the
    // current state, and collapsing `is_set` lets any passed `default`
    // render in the prompt label (`enabled (default: true)`) while
    // keeping the misleading "Current: …" annotation out of the help.
    let is_set = field.kind != PropKind::Bool && !current.is_empty() && current != "<unset>";

    // Surface the docstring as help text above the prompt, and append
    // whichever annotation fits the prompt's state: "Default: X" when
    // the section supplied one and the field is unset, "Current: X"
    // when the config carries a user-set value (non-bool only).
    let mut help = field.description.to_string();
    // List-of-strings fields take comma-separated input. Without this
    // hint users guess and end up entering things like `["alice"]` as
    // raw text — the parser then treats that as one big string element
    // and the saved config is garbage.
    if field.kind == PropKind::StringArray {
        if !help.is_empty() {
            help.push('\n');
        }
        help.push_str("Format: alice,bob or [\"alice\", \"bob\"]. Empty = clear list.");
    }
    if !is_set
        && let Some(d) = default
        && !d.is_empty()
    {
        if !help.is_empty() {
            help.push('\n');
        }
        help.push_str(&format!("Default: {d}. Press Enter to accept."));
    } else if is_set {
        if !help.is_empty() {
            help.push('\n');
        }
        help.push_str(&format!("Current: {current}. Enter to keep."));
    }
    ui.note(&help);

    // Label decorates the short name with the default (when visible) so the
    // value is anchored to the prompt line itself, not just the help text.
    let prompt_label = match (is_set, default) {
        (false, Some(d)) if !d.is_empty() => format!("{short} (default: {d})"),
        _ => short.to_string(),
    };
    let prompt = prompt_label.as_str();

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
            // Prefill priority: config current value > section default > empty.
            // When the user accepts the prefilled default (no edit), we
            // still write it through set_prop so the config records the
            // resolved value rather than leaving it as an implicit fallback.
            let prefill = if is_set {
                Some(current.as_str())
            } else {
                default
            };
            match ui.string(prompt, prefill).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    if (is_set || !new.is_empty()) && new != current {
                        persist(cfg, name, &new).await?;
                    }
                }
            }
        }
        PropKind::StringArray => {
            let prefill = if is_set {
                Some(current.as_str())
            } else {
                default
            };
            // Accepts comma-separated input or the bracketed form from
            // config.toml. Reject malformed brackets — otherwise the
            // parser silently coerces them into a single-element list
            // of garbage.
            loop {
                match ui.string(prompt, prefill).await? {
                    Answer::Back => return Ok(Nav::Back),
                    Answer::Value(new) => {
                        let trimmed = new.trim();
                        if trimmed.starts_with('[') && !parses_as_string_array(trimmed) {
                            ui.note("Invalid array. Use alice,bob or [\"alice\", \"bob\"].");
                            continue;
                        }
                        if (is_set || !new.is_empty()) && new != current {
                            persist(cfg, name, &new).await?;
                        }
                        ui.note("");
                        break;
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
            let current_idx = if is_set {
                variants.iter().position(|v| v == &current)
            } else {
                default.and_then(|d| variants.iter().position(|v| v == d))
            };
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
        PropKind::ObjectArray => {
            // Vec<T> of structs (e.g. mcp.servers). The TUI doesn't have
            // a multi-row sub-form UI; surface this as a JSON-array text
            // input so the field is at least editable from the CLI. The
            // dashboard renders these properly via the per-row editor.
            let prefill = if is_set {
                Some(current.as_str())
            } else {
                default
            };
            match ui.string(prompt, prefill).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    if (is_set || !new.is_empty()) && new != current {
                        persist(cfg, name, &new).await?;
                    }
                }
            }
        }
    }
    Ok(Nav::Done)
}

/// Iterate every field under `prefix` in `prop_fields()` and prompt for each.
/// `excludes` lists leaf field names to skip. `defaults` carries per-field
/// fallback values (e.g. provider-trait defaults) surfaced in the prompt
/// when the field is unset. Rewinds on `Nav::Back`; propagates `Back` to
/// the caller when the user rewinds past the first prompt.
async fn prompt_fields_under(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    prefix: &str,
    excludes: &[&str],
    defaults: &[FieldDefault],
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
        let default = find_default(defaults, &names[i]);
        match prompt_field(cfg, ui, &names[i], default).await? {
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
        "providers" => !cfg.providers.models.is_empty(),
        // `channels.cli: bool` is a default-true scalar that lives directly
        // under `channels.*`, so a bare `starts_with("channels.")` check
        // fires on every fresh install. Require a nested channel config
        // (e.g. `channels.telegram.bot-token`) — anything with a second dot
        // segment — to count as user-driven signal.
        "channels" => cfg.prop_fields().iter().any(|f| {
            f.name
                .strip_prefix("channels.")
                .is_some_and(|rest| rest.contains('.'))
        }),
        "hardware" => cfg.hardware.enabled,
        // Personality has no config-schema fields. The signal is whether
        // the user has authored any of the editable markdown files in
        // their workspace.
        "personality" => EDITABLE_PERSONALITY_FILES
            .iter()
            .any(|f| cfg.workspace_dir.join(f).is_file()),
        // Memory's default backend is "sqlite" and Tunnel's is "none" — both
        // are valid user choices indistinguishable from untouched defaults.
        // Marker-only for these two.
        _ => false,
    }
}

fn is_known_provider_name(provider: &str) -> bool {
    let provider = provider.trim();
    zeroclaw_providers::list_providers().iter().any(|entry| {
        entry.name.eq_ignore_ascii_case(provider)
            || entry
                .aliases
                .iter()
                .any(|alias| alias.eq_ignore_ascii_case(provider))
    })
}

fn openai_compat_models_endpoint(base_url: &str) -> Result<reqwest::Url> {
    let raw = base_url.trim();
    if raw.is_empty() {
        anyhow::bail!("OpenAI-compatible model discovery requires a base URL");
    }

    let mut endpoint = reqwest::Url::parse(raw)
        .with_context(|| format!("OpenAI-compatible base URL is invalid: {raw}"))?;
    if !matches!(endpoint.scheme(), "http" | "https") {
        anyhow::bail!("OpenAI-compatible base URL must use http:// or https://");
    }

    let path = endpoint.path().trim_end_matches('/');
    if path.ends_with("/models") {
        endpoint.set_query(None);
        endpoint.set_fragment(None);
        return Ok(endpoint);
    }

    let suffix = if path.ends_with("/v1") || path.contains("/v1/") {
        "models"
    } else {
        "v1/models"
    };
    let next_path = if path.is_empty() {
        format!("/{suffix}")
    } else {
        format!("{path}/{suffix}")
    };
    endpoint.set_path(&next_path);
    endpoint.set_query(None);
    endpoint.set_fragment(None);
    Ok(endpoint)
}

async fn discover_openai_compat_models(
    base_url: &str,
    api_key: Option<&str>,
) -> Result<Vec<String>> {
    discover_openai_compat_models_with_timeout(base_url, api_key, OPENAI_COMPAT_MODELS_TIMEOUT)
        .await
}

async fn discover_openai_compat_models_with_timeout(
    base_url: &str,
    api_key: Option<&str>,
    timeout: Duration,
) -> Result<Vec<String>> {
    let endpoint = openai_compat_models_endpoint(base_url)?;
    let client = reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .context("failed to build OpenAI-compatible discovery client")?;

    let mut request = client.get(endpoint.clone());
    if let Some(key) = api_key.map(str::trim).filter(|key| !key.is_empty()) {
        request = request.bearer_auth(key);
    }

    let response = request
        .send()
        .await
        .with_context(|| format!("OpenAI-compatible model discovery request failed: {endpoint}"))?;
    let status = response.status();
    if !status.is_success() {
        anyhow::bail!("OpenAI-compatible model discovery failed at {endpoint}: HTTP {status}");
    }

    let payload: OpenAiModelsResponse = response.json().await.with_context(|| {
        format!("OpenAI-compatible model discovery returned invalid JSON: {endpoint}")
    })?;
    let models: Vec<String> = payload
        .data
        .into_iter()
        .map(|model| model.id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    if models.is_empty() {
        anyhow::bail!("OpenAI-compatible model discovery returned no model ids: {endpoint}");
    }
    Ok(models)
}

fn openai_compat_discovery_base_url(
    provider: &str,
    configured_base_url: Option<&str>,
) -> Option<String> {
    configured_base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            provider
                .trim()
                .strip_prefix("custom:")
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(ToString::to_string)
        })
}

async fn prompt_custom_openai_base_url(ui: &mut dyn OnboardUi) -> Result<Option<String>> {
    loop {
        match ui.string("OpenAI-compatible base URL", None).await? {
            Answer::Back => return Ok(None),
            Answer::Value(value) => {
                let normalized = value.trim().trim_end_matches('/').to_string();
                if openai_compat_models_endpoint(&normalized).is_ok() {
                    return Ok(Some(normalized));
                }
                ui.note("Enter an http:// or https:// URL for an OpenAI-compatible API base.");
            }
        }
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
        match prompt_field(cfg, ui, "workspace.enabled", None).await? {
            Nav::Back => return Ok(Nav::Back),
            Nav::Done => {}
        }
        if cfg.workspace.enabled {
            match prompt_fields_under(cfg, ui, "workspace", &["enabled"], &[]).await? {
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
        let current_type = cfg
            .providers
            .first_provider_type()
            .unwrap_or("")
            .to_string();

        let (picked, selected_base_url) = match &flags.provider {
            Some(forced) => (forced.clone(), None),
            None => {
                let current_idx = entries.iter().position(|p| p.name == current_type);
                let mut options: Vec<SelectItem> = entries
                    .iter()
                    .map(|p| {
                        let configured = cfg.providers.models.contains_key(p.name);
                        let is_active = p.name == current_type;
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
                let custom_idx = options.len();
                options.push(SelectItem::new(CUSTOM_OPENAI_COMPAT_LABEL));
                // "Done" lets the user exit providers without picking one —
                // matches the channels picker's escape hatch. Highlight it
                // by default when no fallback is set yet (first-time setup).
                let done_idx = options.len();
                options.push(SelectItem::new("Done"));
                let initial = current_idx.or(Some(done_idx));
                let idx = match ui.select("Provider", &options, initial).await? {
                    Answer::Back => return Ok(Nav::Back),
                    Answer::Value(idx) => idx,
                };
                if idx == done_idx {
                    break;
                }
                if idx == custom_idx {
                    let Some(base_url) = prompt_custom_openai_base_url(ui).await? else {
                        continue;
                    };
                    (format!("custom:{base_url}"), Some(base_url))
                } else {
                    (entries[idx].name.to_string(), None)
                }
            }
        };

        // Seed the HashMap entry in memory so `prop_fields` can enumerate
        // its fields for the prompts below. Not persisted here — the first
        // `persist()` for a real value (api_key, model, …) carries it to
        // disk. If the user backs out before any value is set, the back
        // paths drop the entry so it never reaches the file.
        let is_new_entry = !cfg
            .providers
            .models
            .get(&picked)
            .is_some_and(|m| m.contains_key("default"));
        cfg.providers
            .models
            .entry(picked.clone())
            .or_default()
            .entry("default".to_string())
            .or_default();

        // For fresh entries, pre-populate the provider's trait-level defaults
        // into the in-memory entry. Skipped when reconfiguring so existing
        // user overrides aren't clobbered. Lives in memory until the first
        // `persist()` carries the entry — defaults included — to disk.
        if is_new_entry {
            let prefix = format!("providers.models.{picked}.default");
            field_visibility::apply_provider_trait_defaults(cfg, &picked, &prefix)?;
        }
        if let Some(base_url) = selected_base_url.as_deref() {
            cfg.set_prop(
                &format!("providers.models.{picked}.default.base-url"),
                base_url,
            )?;
        }

        let display_name = entries
            .iter()
            .find(|p| p.name == picked)
            .map(|p| p.display_name)
            .unwrap_or_else(|| {
                if picked.starts_with("custom:") {
                    CUSTOM_OPENAI_COMPAT_LABEL
                } else {
                    picked.as_str()
                }
            });
        ui.heading(2, display_name);

        // Apply CLI-flag overrides up front, then skip those names in the
        // interactive pass so the user isn't re-prompted for what they already
        // passed on the command line.
        let prefix = format!("providers.models.{picked}.default");
        let api_key_path = format!("{prefix}.api-key");
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
            match prompt_field(cfg, ui, &api_key_path, None).await? {
                Nav::Back => {
                    if flags.provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    cfg.providers.models.remove(&picked);
                    continue;
                }
                Nav::Done => {}
            }
            ui.heading(2, display_name);
        }

        if flags.model.is_none() {
            ui.heading(2, &format!("{display_name} › Model"));
            match prompt_model(cfg, ui, &picked).await? {
                Nav::Back => {
                    if flags.provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    cfg.providers.models.remove(&picked);
                    continue;
                }
                Nav::Done => {}
            }
            ui.heading(2, display_name);
        }

        // Advanced settings (temperature, timeout, base-url override,
        // wire-api, etc.) are gated behind an opt-in. Most users never
        // touch these, and the trait-level defaults are sensible.
        match offer_advanced_settings(cfg, ui, &picked, &prefix).await? {
            Nav::Back => {
                if flags.provider.is_some() {
                    return Ok(Nav::Back);
                }
                continue;
            }
            Nav::Done => {}
        }

        break;
    }

    mark_completed(cfg, "providers").await?;
    Ok(Nav::Done)
}

/// Opt-in gate for the per-provider advanced field sweep. Default N so the
/// user breezes through onboarding after auth + model; Y walks them through
/// every remaining field (temperature, max_tokens, timeout_secs, base_url,
/// wire_api, azure_*, etc.) with the provider's trait defaults pre-filled.
async fn offer_advanced_settings(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    provider: &str,
    prefix: &str,
) -> Result<Nav> {
    ui.heading(2, "Advanced settings");
    ui.note(
        "Temperature, timeout, base-URL override, wire protocol, etc. The \
         provider's own defaults are used when these are left unset — skip \
         unless you need to override something specific.",
    );
    match ui.confirm("Configure advanced settings?", false).await? {
        Answer::Back => return Ok(Nav::Back),
        Answer::Value(false) => return Ok(Nav::Done),
        Answer::Value(true) => {}
    }

    let defaults = provider_trait_defaults_for_prompts(provider, prefix);

    // Skipped: `model` (already via prompt_model), `api-key` (explicit auth
    // phase), and fields that only apply to a different provider family
    // (e.g. azure-openai-* when the user picked Anthropic).
    let mut excludes: Vec<&str> = vec!["model", "api-key"];
    excludes.extend(field_visibility::provider_family_excludes(provider));
    prompt_fields_under(cfg, ui, prefix, &excludes, &defaults).await
}

/// Build the `FieldDefault` list for the prompt walker by walking the
/// schema fields of `zeroclaw_providers::default_provider_config(provider)`
/// and rebasing each leaf onto `prefix`. Same source of truth the gateway's
/// `apply_provider_trait_defaults` uses — schema-driven, no per-field
/// names to keep in sync.
fn provider_trait_defaults_for_prompts(provider: &str, prefix: &str) -> Vec<FieldDefault> {
    let defaults = zeroclaw_providers::default_provider_config(provider);
    let source_dot = format!(
        "{}.",
        zeroclaw_config::schema::ModelProviderConfig::configurable_prefix()
    );
    defaults
        .prop_fields()
        .into_iter()
        .filter_map(|field| {
            let leaf = field.name.strip_prefix(&source_dot)?;
            if leaf.contains('.') || field.display_value == "<unset>" {
                return None;
            }
            Some(FieldDefault {
                path: format!("{prefix}.{leaf}"),
                display: field.display_value,
            })
        })
        .collect()
}

/// Prompt for the model field using the provider's live model catalog.
///
/// Calls `Provider::list_models()` (no auth — see `zeroclaw-providers`
/// models_dev + native public endpoints). Falls back to a manual string
/// input when the provider doesn't expose a no-auth list or the fetch fails.
async fn prompt_model(cfg: &mut Config, ui: &mut dyn OnboardUi, provider: &str) -> Result<Nav> {
    let model_path = format!("providers.models.{provider}.default.model");
    let current = cfg.get_prop(&model_path).unwrap_or_default();
    let is_set = !current.is_empty() && current != "<unset>";
    // Resolve profile: support both dotted "type.alias" and bare type key → "default" alias.
    let profile = if let Some((type_k, alias_k)) = provider.split_once('.') {
        cfg.providers
            .models
            .get(type_k)
            .and_then(|m| m.get(alias_k))
    } else {
        cfg.providers
            .models
            .get(provider)
            .and_then(|m| m.get("default"))
    };
    let api_key = profile.and_then(|entry| entry.api_key.as_deref());
    let configured_base_url = profile.and_then(|entry| entry.base_url.as_deref());
    let discovery_base_url = openai_compat_discovery_base_url(provider, configured_base_url);
    let should_try_openai_compat =
        provider.trim().starts_with("custom:") || !is_known_provider_name(provider);

    let catalog_models = match zeroclaw_providers::create_provider(provider, None) {
        Ok(handle) => {
            ui.status("Fetching models...");
            match handle.list_models().await {
                Ok(models) => Some(models),
                Err(e) => {
                    tracing::debug!(provider, error = ?e, "models.dev catalog fetch failed");
                    None
                }
            }
        }
        Err(e) => {
            tracing::debug!(
                provider,
                error = ?e,
                "provider construction failed for model-list probe"
            );
            None
        }
    };
    let live_models = match catalog_models.filter(|ms| !ms.is_empty()) {
        Some(models) => Some(models),
        None if should_try_openai_compat => {
            if let Some(base_url) = discovery_base_url.as_deref() {
                ui.status("Fetching models from /v1/models...");
                match discover_openai_compat_models(base_url, api_key).await {
                    Ok(models) => Some(models),
                    Err(e) => {
                        tracing::debug!(
                            provider,
                            base_url,
                            error = ?e,
                            "OpenAI-compatible model discovery failed"
                        );
                        None
                    }
                }
            } else {
                None
            }
        }
        None => None,
    };

    let new_value = match live_models {
        Some(models) => {
            let items: Vec<SelectItem> = models.iter().map(SelectItem::new).collect();
            let current_idx = models.iter().position(|m| m == &current);
            match ui.select("Model", &items, current_idx).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(idx) => models[idx].clone(),
            }
        }
        None => {
            // Live fetch failed or returned empty (provider doesn't expose
            // a no-auth listing). The underlying error was traced at debug
            // level; surface a short provider-named nudge to the user and
            // fall back to manual entry.
            ui.note(&format!(
                "Catalog lookup failed for {provider} — enter a model id manually \
                 (see the provider's docs for the exact format)."
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
        // Master list of all channels that exist in the schema, derived from
        // the static map_key_sections() metadata. Feature-gated channels drop
        // out automatically because their fields aren't registered.
        let all_channels: Vec<String> = {
            let prefix = "channels.";
            zeroclaw_config::schema::Config::map_key_sections()
                .into_iter()
                .filter_map(|s| {
                    s.path
                        .strip_prefix(prefix)
                        .filter(|rest| !rest.contains('.'))
                        .map(String::from)
                })
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect()
        };
        // A channel type is "configured" if the live config has any prop fields under it.
        let live_fields: Vec<String> = cfg.prop_fields().into_iter().map(|f| f.name).collect();
        let configured: std::collections::BTreeSet<String> = all_channels
            .iter()
            .filter(|name| {
                let prefix = format!("channels.{name}.");
                live_fields.iter().any(|f| f.starts_with(&prefix))
            })
            .cloned()
            .collect();

        let mut options: Vec<SelectItem> = all_channels
            .iter()
            .map(|name| {
                // Match the providers picker's two-tier badge: `[active]`
                // wins when the block exists AND `<channel>.enabled = true`,
                // otherwise `[configured]` for a present-but-disabled block.
                // Web `/onboard` renders the same tiers via
                // `schema_walk_picker` in `crates/zeroclaw-gateway/src/api_onboard.rs`.
                let is_active = live_fields.iter().any(|f| {
                    f.starts_with(&format!("channels.{name}."))
                        && f.ends_with(".enabled")
                        && cfg.get_prop(f).ok().as_deref() == Some("true")
                });
                if is_active {
                    SelectItem::with_badge(name.clone(), "[active]")
                } else if configured.contains(name) {
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
        // Find first existing alias, or create one named after the channel type.
        let alias = cfg
            .prop_fields()
            .into_iter()
            .find_map(|f| {
                f.name
                    .strip_prefix(&format!("channels.{picked}."))
                    .and_then(|rest| rest.split('.').next().map(String::from))
            })
            .unwrap_or_else(|| picked.clone());
        cfg.create_map_key(&format!("channels.{picked}"), &alias)
            .ok();
        let prefix = format!("channels.{picked}.{alias}");
        cfg.save().await?;
        ui.heading(2, picked);
        // Back inside a channel's subfields bounces to the channel list
        // (not to the previous section) — user is still inside Channels.
        let _ = prompt_fields_under(cfg, ui, &prefix, &[], &[]).await?;
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
    let _ = prompt_field(cfg, ui, "memory.auto-save", None).await?;
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
        match prompt_field(cfg, ui, "hardware.enabled", None).await? {
            Nav::Back => return Ok(Nav::Back),
            Nav::Done => {}
        }
        if cfg.hardware.enabled {
            match prompt_fields_under(cfg, ui, "hardware", &["enabled"], &[]).await? {
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
        match prompt_fields_under(cfg, ui, &prefix, &[], &[]).await? {
            Nav::Back => continue,
            Nav::Done => break,
        }
    }
    mark_completed(cfg, "tunnel").await?;
    Ok(Nav::Done)
}

/// Personality — open `$EDITOR` on each markdown file the runtime
/// injects into the system prompt at request time. Files default to
/// the bundled starter template when they don't yet exist on disk;
/// the user is free to overwrite or skip per file. Lives at the end
/// of the flow on purpose: the structural sections (workspace,
/// providers, memory, …) are answered first so the personality files
/// can reference whatever was just configured.
async fn personality(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    ui.heading(1, "Personality");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        "personality",
        "Personality",
        section_has_signal(cfg, "personality"),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }

    let template_ctx = TemplateContext {
        include_memory: cfg.memory.backend.as_str() != "none",
        ..TemplateContext::default()
    };
    let workspace_dir = cfg.workspace_dir.clone();

    loop {
        // Build the picker fresh on every iteration so badges reflect
        // the on-disk state after each edit.
        let mut items: Vec<SelectItem> = EDITABLE_PERSONALITY_FILES
            .iter()
            .map(|filename| {
                let exists = workspace_dir.join(filename).is_file();
                SelectItem::with_badge(
                    (*filename).to_string(),
                    if exists { "saved" } else { "not saved" },
                )
            })
            .collect();
        items.push(SelectItem::new("Done"));

        match ui.select("Personality file to edit", &items, None).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(idx) if idx == EDITABLE_PERSONALITY_FILES.len() => break,
            Answer::Value(idx) => {
                let filename = EDITABLE_PERSONALITY_FILES[idx];
                let path = workspace_dir.join(filename);
                let initial = if path.is_file() {
                    std::fs::read_to_string(&path).unwrap_or_default()
                } else {
                    render_personality(filename, &template_ctx).unwrap_or_default()
                };
                match ui.editor(&format!("Editing {filename}"), &initial).await? {
                    Answer::Back => continue,
                    Answer::Value(content) => {
                        if let Some(parent) = path.parent() {
                            std::fs::create_dir_all(parent)?;
                        }
                        std::fs::write(&path, content)?;
                    }
                }
            }
        }
    }

    mark_completed(cfg, "personality").await?;
    Ok(Nav::Done)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::onboard::ui::quick::QuickUi;
    use axum::Router;
    use axum::http::{StatusCode, header};
    use axum::routing::get;
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::net::TcpListener;
    use zeroclaw_config::schema::{Config, ModelProviderConfig};

    /// Build a `Config` whose `config_path` / `workspace_dir` live inside a
    /// temp directory, so `save()` touches only the scratch tree.
    fn test_cfg(temp: &TempDir) -> Config {
        Config {
            config_path: temp.path().join("config.toml"),
            workspace_dir: temp.path().join("workspace"),
            ..Default::default()
        }
    }

    async fn spawn_models_endpoint(
        status: StatusCode,
        body: &'static str,
        delay: Option<Duration>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = Arc::new(body.to_string());
        let app = Router::new().route(
            "/v1/models",
            get(move || {
                let body = body.clone();
                async move {
                    if let Some(delay) = delay {
                        tokio::time::sleep(delay).await;
                    }
                    (
                        status,
                        [(header::CONTENT_TYPE, "application/json")],
                        body.to_string(),
                    )
                }
            }),
        );
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://localhost:{port}")
    }

    #[tokio::test]
    async fn section_has_signal_workspace_tracks_enabled_flag() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        assert!(!section_has_signal(&cfg, "workspace"));
        cfg.workspace.enabled = true;
        assert!(section_has_signal(&cfg, "workspace"));
    }

    #[tokio::test]
    async fn section_has_signal_providers_requires_models_entry() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        assert!(!section_has_signal(&cfg, "providers"));
        cfg.providers
            .models
            .entry("anthropic".into())
            .or_default()
            .insert("default".to_string(), ModelProviderConfig::default());
        assert!(section_has_signal(&cfg, "providers"));
    }

    #[tokio::test]
    async fn section_has_signal_hardware_tracks_enabled_flag() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        assert!(!section_has_signal(&cfg, "hardware"));
        cfg.hardware.enabled = true;
        assert!(section_has_signal(&cfg, "hardware"));
    }

    #[tokio::test]
    async fn section_has_signal_memory_and_tunnel_are_marker_only() {
        let temp = TempDir::new().unwrap();
        let cfg = test_cfg(&temp);
        // Memory defaults to "sqlite" and Tunnel defaults to "none" — both
        // are valid user choices indistinguishable from untouched defaults,
        // so the completed-sections marker is the only skip-gate signal.
        assert!(!section_has_signal(&cfg, "memory"));
        assert!(!section_has_signal(&cfg, "tunnel"));
    }

    #[tokio::test]
    async fn mark_completed_is_dedupe_safe() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        mark_completed(&mut cfg, "workspace").await.unwrap();
        mark_completed(&mut cfg, "workspace").await.unwrap();
        let count = cfg
            .onboard_state
            .completed_sections
            .iter()
            .filter(|s| s.as_str() == "workspace")
            .count();
        assert_eq!(count, 1, "marker should be inserted at most once");
    }

    #[tokio::test]
    async fn skip_gate_skips_when_marked_and_user_declines() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.onboard_state
            .completed_sections
            .push("workspace".into());

        // QuickUi with no scripted answers returns `default` from `confirm`,
        // which for the reconfigure prompt is `false` → SkipNav::Skip.
        let mut ui = QuickUi::new();
        let result = skip_if_configured(
            &cfg,
            &mut ui,
            &Flags::default(),
            "workspace",
            "Workspace",
            false,
        )
        .await
        .unwrap();
        assert_eq!(result, SkipNav::Skip);
    }

    #[tokio::test]
    async fn skip_gate_skips_when_signal_present_and_user_declines() {
        let temp = TempDir::new().unwrap();
        let cfg = test_cfg(&temp);
        // No marker, but caller reports meaningful config in this section.
        let mut ui = QuickUi::new();
        let result = skip_if_configured(
            &cfg,
            &mut ui,
            &Flags::default(),
            "workspace",
            "Workspace",
            true,
        )
        .await
        .unwrap();
        assert_eq!(result, SkipNav::Skip);
    }

    #[tokio::test]
    async fn skip_gate_enters_when_force_flag_set() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.onboard_state
            .completed_sections
            .push("workspace".into());

        let mut ui = QuickUi::new();
        let flags = Flags {
            force: true,
            ..Default::default()
        };
        let result = skip_if_configured(&cfg, &mut ui, &flags, "workspace", "Workspace", true)
            .await
            .unwrap();
        assert_eq!(result, SkipNav::Enter);
    }

    #[tokio::test]
    async fn skip_gate_enters_when_unmarked_and_no_signal() {
        let temp = TempDir::new().unwrap();
        let cfg = test_cfg(&temp);
        let mut ui = QuickUi::new();
        let result = skip_if_configured(
            &cfg,
            &mut ui,
            &Flags::default(),
            "workspace",
            "Workspace",
            false,
        )
        .await
        .unwrap();
        assert_eq!(result, SkipNav::Enter);
    }

    #[tokio::test]
    async fn discover_openai_compat_models_parses_valid_models_payload() {
        let base_url = spawn_models_endpoint(
            StatusCode::OK,
            r#"{"object":"list","data":[{"id":"llama-3.3"},{"id":" qwen3-coder "}]}"#,
            None,
        )
        .await;

        let models = discover_openai_compat_models(&base_url, Some("sk-test"))
            .await
            .unwrap();

        assert_eq!(models, vec!["llama-3.3", "qwen3-coder"]);
    }

    #[tokio::test]
    async fn discover_openai_compat_models_rejects_malformed_json() {
        let base_url = spawn_models_endpoint(StatusCode::OK, r#"{"data":["#, None).await;

        let err = discover_openai_compat_models(&base_url, Some("sk-test"))
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("invalid JSON"),
            "unexpected discovery error: {err}"
        );
    }

    #[tokio::test]
    async fn discover_openai_compat_models_reports_unauthorized() {
        let base_url =
            spawn_models_endpoint(StatusCode::UNAUTHORIZED, r#"{"error":"bad key"}"#, None).await;

        let err = discover_openai_compat_models(&base_url, Some("sk-test"))
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("HTTP 401"),
            "unexpected discovery error: {err}"
        );
    }

    #[tokio::test]
    async fn discover_openai_compat_models_reports_not_found() {
        let base_url =
            spawn_models_endpoint(StatusCode::NOT_FOUND, r#"{"error":"nope"}"#, None).await;

        let err = discover_openai_compat_models(&base_url, Some("sk-test"))
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("HTTP 404"),
            "unexpected discovery error: {err}"
        );
    }

    #[tokio::test]
    async fn discover_openai_compat_models_reports_server_error() {
        let base_url = spawn_models_endpoint(
            StatusCode::INTERNAL_SERVER_ERROR,
            r#"{"error":"boom"}"#,
            None,
        )
        .await;

        let err = discover_openai_compat_models(&base_url, Some("sk-test"))
            .await
            .unwrap_err()
            .to_string();

        assert!(
            err.contains("HTTP 500"),
            "unexpected discovery error: {err}"
        );
    }

    #[tokio::test]
    async fn discover_openai_compat_models_reports_network_timeout() {
        let base_url = spawn_models_endpoint(
            StatusCode::OK,
            r#"{"data":[{"id":"slow-model"}]}"#,
            Some(Duration::from_millis(200)),
        )
        .await;

        let err = discover_openai_compat_models_with_timeout(
            &base_url,
            Some("sk-test"),
            Duration::from_millis(50),
        )
        .await
        .unwrap_err()
        .to_string();

        assert!(
            err.contains("request failed"),
            "unexpected discovery error: {err}"
        );
    }

    #[tokio::test]
    async fn providers_custom_openai_endpoint_discovers_models() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let base_url = spawn_models_endpoint(
            StatusCode::OK,
            r#"{"data":[{"id":"llama-local"},{"id":"qwen-local"}]}"#,
            None,
        )
        .await;
        let provider = format!("custom:{base_url}");

        let flags = Flags {
            provider: Some(provider.clone()),
            api_key: Some("sk-custom-test".into()),
            ..Default::default()
        };
        let mut ui = QuickUi::new().with("Model", "qwen-local");

        run(&mut cfg, &mut ui, Section::Providers, &flags)
            .await
            .unwrap();

        let model_cfg = cfg
            .providers
            .models
            .get(&provider)
            .and_then(|m| m.get("default"))
            .expect("custom provider entry should be seeded");
        assert_eq!(model_cfg.api_key.as_deref(), Some("sk-custom-test"));
        assert_eq!(model_cfg.model.as_deref(), Some("qwen-local"));
    }

    #[tokio::test]
    async fn prompt_model_unknown_provider_with_base_url_discovers_models() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let base_url = spawn_models_endpoint(
            StatusCode::OK,
            r#"{"data":[{"id":"gateway-small"},{"id":"gateway-large"}]}"#,
            None,
        )
        .await;
        cfg.providers
            .models
            .entry("my-gateway".into())
            .or_default()
            .insert(
                "default".to_string(),
                ModelProviderConfig {
                    api_key: Some("sk-gateway-test".into()),
                    base_url: Some(base_url),
                    ..Default::default()
                },
            );
        let mut ui = QuickUi::new().with("Model", "gateway-large");

        prompt_model(&mut cfg, &mut ui, "my-gateway").await.unwrap();

        let model_cfg = cfg
            .providers
            .models
            .get("my-gateway")
            .and_then(|m| m.get("default"))
            .expect("unknown provider entry should remain configured");
        assert_eq!(model_cfg.model.as_deref(), Some("gateway-large"));
    }

    /// Providers section driven entirely by CLI flags: the `--provider`,
    /// `--api-key`, and `--model` overrides fire up-front, bypassing the
    /// `ui.select` menu, the api-key prompt, and `prompt_model` (which
    /// would otherwise reach out to `models.dev` for the live catalog).
    /// Only the opt-in advanced-settings confirmation remains, and QuickUi
    /// defaults that to `false`.
    #[tokio::test]
    async fn providers_forced_via_flags_persists_and_marks_completed() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);

        let flags = Flags {
            provider: Some("anthropic".into()),
            api_key: Some("sk-ant-test".into()),
            model: Some("claude-opus-4-7".into()),
            ..Default::default()
        };
        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Providers, &flags)
            .await
            .unwrap();

        let model_cfg = cfg
            .providers
            .models
            .get("anthropic")
            .and_then(|m| m.get("default"))
            .expect("anthropic.default entry should be seeded");
        assert_eq!(model_cfg.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(model_cfg.api_key.as_deref(), Some("sk-ant-test"));
        assert!(
            cfg.onboard_state
                .completed_sections
                .iter()
                .any(|s| s == "providers"),
            "providers section should mark completed"
        );
    }

    /// Double-run idempotency for providers: prime via flags, then a
    /// flags-free second run hits the skip-gate (marker + fallback +
    /// models entry = has_signal) and QuickUi's default-false confirm
    /// declines reconfigure, leaving the on-disk config byte-identical.
    #[tokio::test]
    async fn providers_second_run_no_flags_is_idempotent_on_disk() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);

        let prime = Flags {
            provider: Some("anthropic".into()),
            api_key: Some("sk-ant-test".into()),
            model: Some("claude-opus-4-7".into()),
            ..Default::default()
        };
        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Providers, &prime)
            .await
            .unwrap();
        let after_first = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();

        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Providers, &Flags::default())
            .await
            .unwrap();
        let after_second = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();
        assert_eq!(
            after_first, after_second,
            "second run hit the skip-gate and must not rewrite config.toml"
        );
    }

    /// Channels section with no scripted answers: the user falls onto the
    /// pre-selected "Done" option in the channel menu, the section marks
    /// completed, and a second run hits the skip-gate and leaves the file
    /// bytes unchanged.
    #[tokio::test]
    async fn channels_done_selection_is_idempotent_on_disk() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let flags = Flags::default();

        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Channels, &flags)
            .await
            .unwrap();

        assert!(
            cfg.onboard_state
                .completed_sections
                .iter()
                .any(|s| s == "channels"),
            "first run should mark channels completed"
        );
        let after_first = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();

        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Channels, &flags)
            .await
            .unwrap();
        let after_second = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();
        assert_eq!(
            after_first, after_second,
            "second run hit the skip-gate and must not rewrite config.toml"
        );
    }

    /// Smoke test: picking Telegram in the channels menu initializes the
    /// subsection and the scripted bot-token lands via `set_prop`. Covers
    /// the per-channel field-walk path that `channels_done_selection_*`
    /// doesn't exercise (it picks Done immediately).
    #[tokio::test]
    async fn channels_telegram_selection_writes_entry() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let flags = Flags::default();

        let mut ui = QuickUi::new()
            .with("bot-token", "stub-tg-token")
            // Optional Option<String> field with no default — QuickUi's
            // `string` method bails when both answer and current prefill
            // are None. An empty-string answer lets prompt_field's
            // is-set-guard skip the persist, leaving the field None.
            .with("proxy-url", "")
            .with_sequence("Channel", ["telegram", "Done"]);
        run(&mut cfg, &mut ui, Section::Channels, &flags)
            .await
            .unwrap();

        let tg = cfg
            .channels
            .telegram
            .get("default")
            .expect("telegram subsection should be initialized");
        assert_eq!(tg.bot_token, "stub-tg-token");
        assert!(
            cfg.onboard_state
                .completed_sections
                .iter()
                .any(|s| s == "channels"),
            "channels section should mark completed"
        );
    }

    /// Smoke test: picking Mochat walks the enabled-gate fields and the
    /// resulting config has `enabled = true`, the scripted base URL, and
    /// the scripted API token round-tripped via `set_prop`. Doubles as a
    /// regression guard for the orchestrator's mochat enabled-check — a
    /// config with `enabled = true` must reach the registration branch,
    /// one with the default `false` must not.
    #[tokio::test]
    async fn channels_mochat_selection_persists_enabled_url_and_token() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let flags = Flags::default();

        let mut ui = QuickUi::new()
            .with("enabled", "true")
            .with("api-url", "http://mochat-test:8080/v1")
            .with("api-token", "stub-mochat-token")
            .with_sequence("Channel", ["mochat", "Done"]);
        run(&mut cfg, &mut ui, Section::Channels, &flags)
            .await
            .unwrap();

        let mc = cfg
            .channels
            .mochat
            .get("default")
            .expect("mochat subsection should be initialized");
        assert!(mc.enabled, "mochat enabled should round-trip via set_prop");
        assert_eq!(mc.api_url, "http://mochat-test:8080/v1");
        assert_eq!(mc.api_token, "stub-mochat-token");
    }

    /// Acceptance-criteria guarantee: a double run of the same section must
    /// produce identical on-disk TOML. The first run walks the workspace
    /// section with QuickUi's `confirm` defaults (enabled stays false),
    /// marks the section complete, and saves. The second run hits the
    /// skip-gate, the user declines reconfigure (QuickUi default `false`),
    /// and the section returns without writing.
    #[tokio::test]
    async fn workspace_double_run_is_idempotent_on_disk() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let flags = Flags::default();

        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Workspace, &flags)
            .await
            .unwrap();

        assert!(
            cfg.onboard_state
                .completed_sections
                .iter()
                .any(|s| s == "workspace"),
            "first run should mark workspace completed"
        );
        let after_first = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();

        let mut ui = QuickUi::new();
        run(&mut cfg, &mut ui, Section::Workspace, &flags)
            .await
            .unwrap();
        let after_second = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();
        assert_eq!(
            after_first, after_second,
            "second run hit the skip-gate and must not rewrite config.toml"
        );
    }
}
