//! Onboard orchestrator.
//!
//! Thin dispatcher above the `OnboardUi` trait (defined in
//! `zeroclaw-config::traits`). Section-scoped entry points let callers run
//! just one slice (`zeroclaw onboard channels`) or the whole flow.
//!
//! Everything writes through `Config::set_prop` (or its helpers); direct
//! struct-field assignment is off-limits per the DRY contract.

use std::time::Duration;

use anyhow::{Context, Result};
use serde::Deserialize;
use zeroclaw_config::schema::Config;
use zeroclaw_config::traits::{Answer, OnboardUi, PropKind, SelectItem};

const CUSTOM_OPENAI_COMPAT_LABEL: &str = "Custom OpenAI-compatible endpoint";
const OPENAI_COMPAT_MODELS_TIMEOUT: Duration = Duration::from_secs(10);

/// Sections without a tailored interactive wizard. Single source for
/// the variant list used by `dispatch_section` and `section_has_signal`.
/// Macros expand pre-typecheck so each match stays exhaustive.
macro_rules! acknowledge_only_sections {
    () => {
        Section::Storage
            | Section::Cron
            | Section::Mcp
            | Section::McpBundles
            | Section::KnowledgeBundles
    };
}

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

pub use zeroclaw_config::sections::Section;

/// What slice of onboarding the orchestrator should run. `None` walks
/// the full wizard (every [`Section`] in canonical order); `Some(s)`
/// targets one section. The runtime intentionally has no parallel
/// `Section`-with-`All` enum — that was three drift surfaces in one
/// file.
pub type Target = Option<Section>;

/// First segment of a dotted property path mapped back to the wizard
/// section it lives under, or `None` for non-wizard paths
/// (`onboard_state.completed_sections`, etc.).
#[must_use]
pub fn section_for_path(path: &str) -> Option<Section> {
    Section::from_key(path.split('.').next()?)
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
    pub model_provider: Option<String>,
    pub model: Option<String>,
    pub memory: Option<String>,
}

/// Top-level onboard dispatcher. `target` is the canonical
/// `Option<Section>` from `zeroclaw_runtime::onboard::Target` —
/// `None` walks the full wizard, `Some(s)` runs a single section.
pub async fn run(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    target: Target,
    flags: &Flags,
) -> Result<()> {
    let Some(section) = target else {
        return run_all(cfg, ui, flags).await;
    };
    let _ = dispatch_section(cfg, ui, flags, section).await?;
    Ok(())
}

/// Run a single onboarding section. Returns `Nav::Done` for sections
/// without an interactive wizard (the operator reaches them via the
/// `/config` dashboard or `zeroclaw config set`); the run-all loop
/// treats Done as "advance to the next section". Exhaustive over
/// [`Section`] so adding a variant to the canonical enum forces a
/// match arm here.
async fn dispatch_section(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    flags: &Flags,
    section: Section,
) -> Result<Nav> {
    // Each arm's future is `Box::pin`'d so the enclosing state machine
    // holds a small pointer, not the inlined union of every section
    // handler's locals. Without this the test thread's 2 MiB stack
    // overflows on the deepest interactive wizards (notably the model-
    // provider model-discovery path).
    match section {
        Section::ModelProviders => Box::pin(model_providers(cfg, ui, flags)).await,
        Section::TtsProviders | Section::TranscriptionProviders => {
            Box::pin(no_wizard_acknowledge(
                ui,
                section,
                &format!(
                    "No interactive wizard yet. Configure via the dashboard at \
                 /config/{section} or \
                 `zeroclaw config set {section}.<type>.<alias>.<field> <value>`."
                ),
            ))
            .await
        }
        Section::Channels => Box::pin(channels(cfg, ui, flags)).await,
        Section::Memory => Box::pin(memory(cfg, ui, flags)).await,
        Section::Hardware => Box::pin(hardware(cfg, ui, flags)).await,
        Section::Tunnel => Box::pin(tunnel(cfg, ui, flags)).await,
        Section::Agents => Box::pin(agents(cfg, ui, flags)).await,
        Section::Skills => Box::pin(skills(cfg, ui, flags)).await,
        Section::SkillBundles => {
            Box::pin(one_tier_alias_section(
                cfg,
                ui,
                section,
                "skill-bundles",
                "Skill bundle",
            ))
            .await
        }
        Section::RiskProfiles => {
            Box::pin(one_tier_alias_section(
                cfg,
                ui,
                section,
                "risk-profiles",
                "Risk profile",
            ))
            .await
        }
        Section::RuntimeProfiles => {
            Box::pin(one_tier_alias_section(
                cfg,
                ui,
                section,
                "runtime-profiles",
                "Runtime profile",
            ))
            .await
        }
        Section::PeerGroups => {
            Box::pin(one_tier_alias_section(
                cfg,
                ui,
                section,
                "peer-groups",
                "Peer group",
            ))
            .await
        }
        acknowledge_only_sections!() => {
            Box::pin(no_wizard_acknowledge(
                ui,
                section,
                &format!(
                    "Configured via the dashboard at /config/{section} or \
                 `zeroclaw config set {section}.<alias>.<field> <value>` \
                 (not part of the initial wizard)."
                ),
            ))
            .await
        }
    }
}

/// Render heading + help + a confirm prompt for sections without a
/// tailored wizard. Ratatui's `note()` is a no-op without a subsequent
/// prompt to anchor it, so the confirm makes the message actually render.
async fn no_wizard_acknowledge(
    ui: &mut dyn OnboardUi,
    section: Section,
    explanation: &str,
) -> Result<Nav> {
    let mut label = section.as_str().replace(['_', '-', '.'], " ");
    if let Some(c) = label.get_mut(0..1) {
        c.make_ascii_uppercase();
    }
    ui.heading(1, &label);
    let canonical = section.help();
    let note = if canonical.is_empty() {
        explanation.to_string()
    } else {
        format!("{canonical}\n\n{explanation}")
    };
    ui.note(&note);
    let _ = ui.confirm("Continue", false).await?;
    Ok(Nav::Done)
}

/// Walk every section in order with section-level Back. Each section returns
/// `Nav::Back` when the user pressed Esc at its first prompt; the loop
/// rewinds to the previous section. Back at the first section exits
/// onboarding cleanly (user bails out).
async fn run_all(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<()> {
    let order: Vec<Section> = zeroclaw_config::sections::ONBOARDING_WIZARD.to_vec();
    let mut i: usize = 0;
    loop {
        let Some(section) = order.get(i).copied() else {
            return Ok(());
        };
        match dispatch_section(cfg, ui, flags, section).await? {
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
    cfg.set_prop_persistent(path, value)?;
    cfg.save_dirty().await?;
    Ok(())
}

/// Emit the section's heading + help blurb in the canonical layout.
/// Help copy lives on `Section::help()` in `zeroclaw-config` so the
/// CLI / TUI / dashboard render the same text without parallel tables.
/// `display_label` lets per-section code keep its preferred title casing
/// (the canonical key would otherwise be e.g. `model_providers`).
fn emit_section_header(ui: &mut dyn OnboardUi, section: Section, display_label: &str) {
    ui.heading(1, display_label);
    let help = section.help();
    if !help.is_empty() {
        ui.note(help);
    }
}

// ── Field-driven helpers ─────────────────────────────────────────────────

/// Per-field default override. When a section knows a sensible default
/// that lives outside the config (e.g. `AnthropicModelProvider::default_temperature()`),
/// it builds a list of these and passes them to `prompt_fields_under`.
/// The prompt surfaces the default as ghost-text inside the input box
/// plus a "Default: X. Press Enter to accept." line in the help blurb,
/// only when the field is unset in cfg.
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

/// Multi-line pretty form of a JSON-shaped Object scalar for `$EDITOR`
/// hand-off. Returns `None` when the input doesn't parse as JSON so the
/// caller falls through to the raw value (e.g. when the field is still
/// a placeholder string).
fn pretty_print_object(value: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(value.trim()).ok()?;
    serde_json::to_string_pretty(&parsed).ok()
}

/// Compact form of an Object scalar suitable for `set_prop`. Round-trips
/// through `serde_json` so trailing whitespace, comments, and blank lines
/// the user added during editing are normalised away.
fn compact_object(edited: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(edited.trim()).ok()?;
    serde_json::to_string(&parsed).ok()
}

/// True when `input` parses as the same `Vec<String>` form `config.toml`
/// emits. Lets the StringArray prompt accept the bracketed display form
/// bidirectionally.
/// Live alias list to render as a picker when the field's `type_hint`
/// references a typed alias-ref newtype (ChannelRef, AgentAlias, …).
/// `None` for plain `String` fields so the caller falls through to the
/// free-text input.
fn alias_options_for_type_hint(cfg: &Config, type_hint: &str) -> Option<Vec<String>> {
    let dotted = |prefix: &str| -> Vec<String> {
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
    };
    let bare = |section: &str| -> Vec<String> { cfg.get_map_keys(section).unwrap_or_default() };
    if type_hint.contains("ChannelRef") {
        Some(dotted("channels"))
    } else if type_hint.contains("ModelProviderRef") {
        Some(dotted("providers.models"))
    } else if type_hint.contains("TtsProviderRef") {
        Some(dotted("providers.tts"))
    } else if type_hint.contains("TranscriptionProviderRef") {
        Some(dotted("providers.transcription"))
    } else if type_hint.contains("AgentAlias") {
        Some(bare("agents"))
    } else {
        None
    }
}

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
    // Field populated by a `ZEROCLAW_*` env override at load time — show the
    // env-var name and the TOML path, then skip the prompt. The note clears
    // when navigation moves to next/previous step.
    if cfg.prop_is_env_overridden(name) {
        let env_var = format!("ZEROCLAW_{}", name.replace('.', "__").replace('-', "_"),);
        ui.note(&format!(
            "\u{1f489} {name}\n\
             overridden by env: {env_var}\n\
             config.toml path: [{name}] — skipping prompt, value sourced from environment.",
        ));
        return Ok(Nav::Done);
    }

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

    let prompt = short;

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
            // Typed alias-ref (ChannelRef, AgentAlias, ModelProviderRef,
            // TtsProviderRef, TranscriptionProviderRef) — render the
            // live alias list as a select instead of a free-text input.
            // Matches what the dashboard does and keeps the CLI surface
            // from accepting silently-broken refs.
            if let Some(options) = alias_options_for_type_hint(cfg, field.type_hint) {
                let items: Vec<SelectItem> = options.iter().map(SelectItem::new).collect();
                let current_idx = if is_set {
                    options.iter().position(|v| v == &current)
                } else {
                    default.and_then(|d| options.iter().position(|v| v == d))
                };
                match ui.select(prompt, &items, current_idx).await? {
                    Answer::Back => return Ok(Nav::Back),
                    Answer::Value(idx) => {
                        let new = options[idx].clone();
                        if (is_set || !new.is_empty()) && new != current {
                            persist(cfg, name, &new).await?;
                        }
                    }
                }
                return Ok(Nav::Done);
            }
            // `current` pre-fills the buffer (edit mode); `placeholder`
            // renders the section default as ghost text. Enter on empty
            // commits the placeholder.
            let (prefill, placeholder) = if is_set {
                (Some(current.as_str()), None)
            } else {
                (None, default)
            };
            match ui.string(prompt, prefill, placeholder).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    if (is_set || !new.is_empty()) && new != current {
                        persist(cfg, name, &new).await?;
                    }
                }
            }
        }
        PropKind::StringArray => {
            let (prefill, placeholder) = if is_set {
                (Some(current.as_str()), None)
            } else {
                (None, default)
            };
            // Accepts comma-separated input or the bracketed form from
            // config.toml. Reject malformed brackets — otherwise the
            // parser silently coerces them into a single-element list
            // of garbage.
            loop {
                match ui.string(prompt, prefill, placeholder).await? {
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
            let (prefill, placeholder) = if is_set {
                (Some(current.as_str()), None)
            } else {
                (None, default)
            };
            match ui.string(prompt, prefill, placeholder).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    if (is_set || !new.is_empty()) && new != current {
                        persist(cfg, name, &new).await?;
                    }
                }
            }
        }
        PropKind::Object => {
            // Struct-shaped scalar (e.g. agents.<a>.workspace.access — a
            // BTreeMap<AgentAlias, AccessMode>; or model_providers.<id>.pricing).
            // Maps and structs are awkward as single-line input, so hand
            // them to $EDITOR via the OnboardUi editor surface. RatatuiUi
            // suspends, spawns $EDITOR with the current JSON value
            // pretty-printed, and resumes on save — same key/value flow as
            // editing any config file by hand. The dashboard renders a
            // proper structured form via PropKind::Object.
            let initial = if is_set {
                pretty_print_object(&current).unwrap_or_else(|| current.clone())
            } else {
                default.map(str::to_string).unwrap_or_default()
            };
            let hint = format!(
                "Editing {name}. Save and exit to apply, or quit without saving to keep the current value."
            );
            match ui.editor(&hint, &initial).await? {
                Answer::Back => return Ok(Nav::Back),
                Answer::Value(new) => {
                    let normalized = compact_object(&new).unwrap_or_else(|| new.trim().to_string());
                    if (is_set || !normalized.is_empty()) && normalized != current {
                        persist(cfg, name, &normalized).await?;
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
/// has-meaningful-config signal (e.g. providers has a fallback + api-key
/// set). `--force` bypasses unconditionally.
async fn skip_if_configured(
    cfg: &Config,
    ui: &mut dyn OnboardUi,
    flags: &Flags,
    section: Section,
    label: &str,
    has_signal: bool,
) -> Result<SkipNav> {
    if flags.force {
        return Ok(SkipNav::Enter);
    }
    let key = section.as_str();
    let seen = cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|s| s == key);
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
/// from `Config::default()`'s idle state). Exhaustive over [`Section`]
/// so adding a wizard variant forces a decision here.
fn section_has_signal(cfg: &Config, section: Section) -> bool {
    match section {
        Section::ModelProviders => !cfg.providers.models.is_empty(),
        // `channels.cli: bool` is a default-true scalar that lives directly
        // under `channels.*`, so a bare `starts_with("channels.")` check
        // fires on every fresh install. Require a nested channel config
        // (e.g. `channels.telegram.bot-token`) — anything with a second dot
        // segment — to count as user-driven signal.
        Section::Channels => cfg.prop_fields().iter().any(|f| {
            f.name
                .strip_prefix("channels.")
                .is_some_and(|rest| rest.contains('.'))
        }),
        Section::Hardware => cfg.hardware.enabled,
        // Memory's default backend is "sqlite" and Tunnel's is "none" —
        // both are valid user choices indistinguishable from untouched
        // defaults. TTS / transcription providers and agents start
        // empty; their existence in the typed family map IS the signal,
        // not a derivable default-divergence. Marker-only for these.
        Section::TtsProviders
        | Section::TranscriptionProviders
        | Section::Memory
        | Section::Tunnel
        | Section::Agents
        | Section::Skills
        | Section::SkillBundles
        | Section::RiskProfiles
        | Section::RuntimeProfiles
        | Section::PeerGroups => false,
        acknowledge_only_sections!() => false,
    }
}

fn is_known_model_provider_name(model_provider: &str) -> bool {
    let model_provider = model_provider.trim();
    zeroclaw_providers::list_model_providers()
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case(model_provider))
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
    model_provider: &str,
    configured_base_url: Option<&str>,
) -> Option<String> {
    configured_base_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(ToString::to_string)
        .or_else(|| {
            model_provider
                .trim()
                .strip_prefix("custom:")
                .map(str::trim)
                .filter(|url| !url.is_empty())
                .map(ToString::to_string)
        })
}

async fn prompt_custom_openai_base_url(ui: &mut dyn OnboardUi) -> Result<Option<String>> {
    loop {
        match ui.string("OpenAI-compatible base URL", None, None).await? {
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
/// Prompt for an alias name, validating it in a loop until the user enters a
/// valid value or backs out. Returns `Some(alias)` on success, `None` on Back.
async fn prompt_alias_name(ui: &mut dyn OnboardUi, suggestion: &str) -> Result<Option<String>> {
    loop {
        // Alias suggestion is a default the user can accept by hitting
        // Enter, not a pre-filled string to edit — surface it as the
        // ghost-text placeholder so the input box is otherwise empty.
        match ui
            .string(
                "Alias (name for this configuration)",
                None,
                Some(suggestion),
            )
            .await?
        {
            Answer::Back => return Ok(None),
            Answer::Value(s) => {
                let trimmed = if s.trim().is_empty() {
                    suggestion.to_string()
                } else {
                    s.trim().to_string()
                };
                match zeroclaw_config::helpers::validate_alias_key(&trimmed) {
                    Ok(()) => return Ok(Some(trimmed)),
                    Err(msg) => ui.warn(&format!("Invalid alias: {msg}")),
                }
            }
        }
    }
}

async fn mark_completed(cfg: &mut Config, section: Section) -> Result<()> {
    let key = section.as_str();
    if cfg
        .onboard_state
        .completed_sections
        .iter()
        .any(|s| s == key)
    {
        return Ok(());
    }
    cfg.onboard_state.completed_sections.push(key.to_string());
    cfg.mark_dirty("onboard-state.completed-sections");
    cfg.save_dirty().await?;
    Ok(())
}

// ── Sections ─────────────────────────────────────────────────────────────
// Each section returns `Nav::Back` when the user hits Esc at the very first
// prompt. Back from a later prompt within the section rewinds locally (via
// prompt_fields_under / per-section loop), never propagates to the parent.

async fn model_providers(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::ModelProviders, "Providers");

    // Menu is driven by zeroclaw_providers::list_model_providers() — single source
    // of truth for canonical names, display names, aliases.
    let entries = zeroclaw_providers::list_model_providers();

    loop {
        let current_type = cfg.first_model_provider_type().unwrap_or("").to_string();

        let (picked, selected_base_url) = match &flags.model_provider {
            Some(forced) => (forced.clone(), None),
            None => {
                let current_idx = entries.iter().position(|p| p.name == current_type);
                let mut options: Vec<SelectItem> = entries
                    .iter()
                    .map(|p| {
                        let configured = cfg.providers.models.contains_model_provider_type(p.name);
                        // "configured" describes the actual provider state —
                        // at least one alias entry exists in
                        // `[providers.models.<type>]`. The cursor already
                        // shows which row the operator is hovering, so a
                        // second "[active]" badge for current selection
                        // was redundant and misleading.
                        let badge = configured.then(|| "[configured]".into());
                        SelectItem {
                            label: p.display_name.to_string(),
                            badge,
                        }
                    })
                    .collect();
                let custom_idx = options.len();
                options.push(SelectItem::new(CUSTOM_OPENAI_COMPAT_LABEL));
                // "Done" lets the user exit model_providers without picking one —
                // matches the channels picker's escape hatch. Highlight it
                // by default when no fallback is set yet (first-time setup).
                let done_idx = options.len();
                options.push(SelectItem::new("Done"));
                let initial = current_idx.or(Some(done_idx));
                let idx = match ui.select("ModelProvider", &options, initial).await? {
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
                    ("custom".to_string(), Some(base_url))
                } else {
                    (entries[idx].name.to_string(), None)
                }
            }
        };

        // Anchor the breadcrumb to the chosen provider as early as
        // possible — every prompt that follows (alias, auth, model,
        // advanced settings) reads against this header. Without the
        // up-front set, the alias prompt rendered under a generic
        // "Providers" breadcrumb with no provider-name context.
        let display_name = entries
            .iter()
            .find(|p| p.name == picked)
            .map(|p| p.display_name)
            .unwrap_or_else(|| {
                if picked == "custom" {
                    CUSTOM_OPENAI_COMPAT_LABEL
                } else {
                    picked.as_str()
                }
            });
        ui.heading(2, display_name);

        // When --model-provider is forced via CLI flags skip the alias prompt.
        // Otherwise show existing aliases as a selectable list with "+ Add new".
        let alias = if flags.model_provider.is_some() {
            "default".to_string()
        } else {
            let existing_aliases: Vec<String> = cfg
                .get_map_keys(&format!("providers.models.{picked}"))
                .unwrap_or_default();
            if existing_aliases.is_empty() {
                // Override the API-key help text inherited from the
                // section intro with alias-specific guidance.
                ui.note(&format!(
                    "Short identifier for this {display_name} configuration. \
                     Letters, digits, underscores. Empty = use the suggested default."
                ));
                let Some(a) = prompt_alias_name(ui, "default").await? else {
                    continue;
                };
                a
            } else {
                let mut alias_options: Vec<SelectItem> = existing_aliases
                    .iter()
                    .map(|a| SelectItem::new(a.clone()))
                    .collect();
                let add_new_idx = alias_options.len();
                alias_options.push(SelectItem::new("+ Add new"));
                let alias_idx = match ui.select("Alias", &alias_options, Some(0)).await? {
                    Answer::Back => continue,
                    Answer::Value(i) => i,
                };
                if alias_idx == add_new_idx {
                    ui.note(&format!(
                        "Short identifier for this {display_name} configuration. \
                         Letters, digits, underscores. Empty = use the suggested default."
                    ));
                    let suggestion = format!("{}-2", existing_aliases[0]);
                    let Some(a) = prompt_alias_name(ui, &suggestion).await? else {
                        continue;
                    };
                    a
                } else {
                    existing_aliases[alias_idx].clone()
                }
            }
        };

        // Seed the HashMap entry in memory so `prop_fields` can enumerate
        // its fields for the prompts below. Not persisted here — the first
        // `persist()` for a real value (api_key, model, …) carries it to
        // disk. If the user backs out before any value is set, the back
        // paths drop the entry so it never reaches the file.
        let is_new_entry = cfg.providers.models.find(&picked, &alias).is_none();
        cfg.providers.models.ensure(&picked, &alias);

        // Per-family typed configs now derive their own default endpoint
        // URI via the `ModelEndpoint` trait at runtime construction time.
        // The pre-Phase-6 `apply_provider_trait_defaults` walk that copied
        // `default_provider_config` field values into the new entry is gone
        // — operator-set fields ride the typed config's `Default` impl, and
        // family endpoint resolution happens family-side rather than via
        // pre-populated entry fields.
        if let Some(base_url) = selected_base_url.as_deref() {
            cfg.set_prop_persistent(&format!("providers.models.{picked}.{alias}.uri"), base_url)?;
        }

        // (display_name + heading set up-front, immediately after the
        // provider type was picked, so the alias prompt also sees it.)

        // Apply CLI-flag overrides up front, then skip those names in the
        // interactive pass so the user isn't re-prompted for what they already
        // passed on the command line.
        let prefix = format!("providers.models.{picked}.{alias}");
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
        // model_provider subsection so the panel reads "Providers › Authentication".
        if flags.api_key.is_none() {
            ui.heading(2, &format!("{display_name} › Authentication"));
            match prompt_field(cfg, ui, &api_key_path, None).await? {
                Nav::Back => {
                    if flags.model_provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    if is_new_entry {
                        cfg.providers.models.remove_alias(&picked, &alias);
                        cfg.mark_dirty(&format!("providers.models.{picked}.{alias}"));
                    }
                    continue;
                }
                Nav::Done => {}
            }
            ui.heading(2, display_name);
        }

        if flags.model.is_none() {
            ui.heading(2, &format!("{display_name} › Model"));
            match prompt_model(cfg, ui, &prefix).await? {
                Nav::Back => {
                    if flags.model_provider.is_some() {
                        return Ok(Nav::Back);
                    }
                    if is_new_entry {
                        cfg.providers.models.remove_alias(&picked, &alias);
                        cfg.mark_dirty(&format!("providers.models.{picked}.{alias}"));
                    }
                    continue;
                }
                Nav::Done => {}
            }
            ui.heading(2, display_name);
        }

        // Advanced settings (temperature, timeout, base-url override,
        // wire-api, etc.) are gated behind an opt-in. Most users never
        // touch these, and the trait-level defaults are sensible.
        match offer_advanced_settings(cfg, ui, &prefix).await? {
            Nav::Back => {
                if flags.model_provider.is_some() {
                    return Ok(Nav::Back);
                }
                continue;
            }
            Nav::Done => {}
        }

        break;
    }

    mark_completed(cfg, Section::ModelProviders).await?;
    Ok(Nav::Done)
}

/// Opt-in gate for the per-provider advanced field sweep. Default N so the
/// user breezes through onboarding after auth + model; Y walks them through
/// every remaining field (temperature, max_tokens, timeout_secs, base_url,
/// wire_api, azure_*, etc.) with the model_provider's trait defaults pre-filled.
async fn offer_advanced_settings(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    prefix: &str,
) -> Result<Nav> {
    ui.heading(2, "Advanced settings");
    ui.note(
        "Temperature, timeout, base-URL override, wire protocol, etc. The \
         model_provider's own defaults are used when these are left unset — skip \
         unless you need to override something specific.",
    );
    match ui.confirm("Configure advanced settings?", false).await? {
        Answer::Back => return Ok(Nav::Back),
        Answer::Value(false) => return Ok(Nav::Done),
        Answer::Value(true) => {}
    }

    // Per-family typed configs only carry their own family-applicable fields,
    // so no per-family exclude list is needed (vs. pre-#6273 when one flat
    // ModelProviderConfig had every family's fields jumbled together).
    // Excluded: `model` (already prompted via prompt_model) and `api-key`
    // (explicit auth phase).
    let excludes: Vec<&str> = vec!["model", "api-key"];

    // Surface per-field defaults as ghost-text placeholders so the
    // operator sees "this is what we'll use if you hit Enter" instead
    // of an empty box. URI default comes from the family's
    // `FamilyEndpoint::endpoint_uri()` impl; temperature/timeout have
    // hardcoded sensible values that match the runtime fallbacks in
    // the provider factory.
    let mut defaults: Vec<FieldDefault> = Vec::new();
    if let Some((type_k, alias_k)) = prefix
        .strip_prefix("providers.models.")
        .and_then(|rest| rest.split_once('.'))
    {
        // `resolved_endpoint_uri` only returns Some for multi-region
        // families; fall back to the family's canonical default.
        let uri = cfg
            .providers
            .models
            .resolved_endpoint_uri(type_k, alias_k)
            .map(str::to_string)
            .or_else(|| zeroclaw_providers::default_model_provider_url(type_k).map(str::to_string));
        if let Some(uri) = uri {
            defaults.push(FieldDefault {
                path: format!("{prefix}.uri"),
                display: uri,
            });
        }
    }
    defaults.push(FieldDefault {
        path: format!("{prefix}.temperature"),
        display: "0.7".to_string(),
    });
    defaults.push(FieldDefault {
        path: format!("{prefix}.timeout-secs"),
        display: "120".to_string(),
    });

    prompt_fields_under(cfg, ui, prefix, &excludes, &defaults).await
}

/// Prompt for the model field using the model_provider's live model catalog.
///
/// Calls `ModelProvider::list_models()` (no auth — see `zeroclaw-providers`
/// models_dev + native public endpoints). Falls back to a manual string
/// input when the model_provider doesn't expose a no-auth list or the fetch fails.
/// `prefix` is the full alias-level path: `model_providers.<type>.<alias>`.
async fn prompt_model(cfg: &mut Config, ui: &mut dyn OnboardUi, prefix: &str) -> Result<Nav> {
    let model_path = format!("{prefix}.model");
    let current = cfg.get_prop(&model_path).unwrap_or_default();
    let is_set = !current.is_empty() && current != "<unset>";
    // Extract type and alias from "providers.models.<type>.<alias>".
    let (model_provider, profile) = match prefix.strip_prefix("providers.models.") {
        Some(rest) => {
            if let Some((type_k, alias_k)) = rest.split_once('.') {
                let profile = cfg.providers.models.find(type_k, alias_k);
                (type_k.to_string(), profile)
            } else {
                (rest.to_string(), None)
            }
        }
        None => (prefix.to_string(), None),
    };
    let api_key = profile.and_then(|entry| entry.api_key.as_deref());
    let configured_uri = profile.and_then(|entry| entry.uri.as_deref());
    let discovery_base_url = openai_compat_discovery_base_url(&model_provider, configured_uri);
    let should_try_openai_compat =
        model_provider.trim() == "custom" || !is_known_model_provider_name(&model_provider);

    let catalog_models = match zeroclaw_providers::create_model_provider(&model_provider, None) {
        Ok(handle) => {
            ui.status("Fetching models...");
            match handle.list_models().await {
                Ok(models) => Some(models),
                Err(e) => {
                    ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": model_provider, "error": e.to_string()})), "models.dev catalog fetch failed");
                    None
                }
            }
        }
        Err(e) => {
            ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": model_provider, "error": e.to_string()})), "model_provider construction failed for model-list probe");
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
                        ::zeroclaw_log::record!(DEBUG, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_attrs(::serde_json::json!({"model_provider": model_provider, "base_url": base_url, "error": e.to_string()})), "OpenAI-compatible model discovery failed");
                        None
                    }
                }
            } else {
                None
            }
        }
        None => None,
    };
    // Both fetch paths above are best-effort; clear the "Fetching..."
    // status so it doesn't linger as a stale banner in the TUI log
    // pane once the user has moved past the model picker.
    ui.status("");

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
            // Live fetch failed or returned empty (model_provider doesn't expose
            // a no-auth listing). The underlying error was traced at debug
            // level; surface a short provider-named nudge to the user and
            // fall back to manual entry.
            ui.note(&format!(
                "Catalog lookup failed for {model_provider} — enter a model id manually \
                 (see the model_provider's docs for the exact format)."
            ));
            let prefill = if is_set { Some(current.as_str()) } else { None };
            match ui.string("Model id", prefill, None).await? {
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

async fn channels(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Channels, "Channels");
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
                // Match the model_providers picker's two-tier badge: `[active]`
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
        // Show existing aliases as selectable; "+ Add new" appended at the end.
        let existing_aliases: Vec<String> = cfg
            .get_map_keys(&format!("channels.{picked}"))
            .unwrap_or_default();
        let alias = if existing_aliases.is_empty() {
            let Some(a) = prompt_alias_name(ui, "default").await? else {
                continue;
            };
            a
        } else {
            let mut alias_options: Vec<SelectItem> = existing_aliases
                .iter()
                .map(|a| SelectItem::new(a.clone()))
                .collect();
            let add_new_idx = alias_options.len();
            alias_options.push(SelectItem::new("+ Add new"));
            let alias_idx = match ui.select("Alias", &alias_options, Some(0)).await? {
                Answer::Back => continue,
                Answer::Value(i) => i,
            };
            if alias_idx == add_new_idx {
                let suggestion = format!("{}-2", existing_aliases[0]);
                let Some(a) = prompt_alias_name(ui, &suggestion).await? else {
                    continue;
                };
                a
            } else {
                existing_aliases[alias_idx].clone()
            }
        };
        cfg.create_map_key(&format!("channels.{picked}"), &alias)
            .ok();
        let prefix = format!("channels.{picked}.{alias}");
        cfg.mark_dirty(&prefix);
        cfg.save_dirty().await?;
        ui.heading(2, picked);
        // Back inside a channel's subfields bounces to the channel list
        // (not to the previous section) — user is still inside Channels.
        let _ = prompt_fields_under(cfg, ui, &prefix, &[], &[]).await?;
    }
    mark_completed(cfg, Section::Channels).await?;
    Ok(Nav::Done)
}

async fn memory(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Memory, "Memory");
    if flags.memory.is_none() {
        match skip_if_configured(
            cfg,
            ui,
            flags,
            Section::Memory,
            "Memory",
            section_has_signal(cfg, Section::Memory),
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
    mark_completed(cfg, Section::Memory).await?;
    Ok(Nav::Done)
}

async fn hardware(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Hardware, "Hardware");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        Section::Hardware,
        "Hardware",
        section_has_signal(cfg, Section::Hardware),
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
    mark_completed(cfg, Section::Hardware).await?;
    Ok(Nav::Done)
}

async fn tunnel(cfg: &mut Config, ui: &mut dyn OnboardUi, flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Tunnel, "Tunnel");
    match skip_if_configured(
        cfg,
        ui,
        flags,
        Section::Tunnel,
        "Tunnel",
        section_has_signal(cfg, Section::Tunnel),
    )
    .await?
    {
        SkipNav::Skip => return Ok(Nav::Done),
        SkipNav::Back => return Ok(Nav::Back),
        SkipNav::Enter => {}
    }

    loop {
        // ModelProvider list derived from the schema: each `tunnel.<name>.*` field
        // in prop_fields() names a real model_provider. "none" is always valid and
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
        let current_model_provider = cfg.tunnel.tunnel_provider.clone();
        let current_idx = provider_names
            .iter()
            .position(|p| p == &current_model_provider);
        let idx = match ui
            .select("Public tunnel model_provider", &options, current_idx)
            .await?
        {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) => i,
        };
        let new_model_provider = provider_names[idx].clone();

        if new_model_provider != current_model_provider {
            persist(cfg, "tunnel.tunnel-provider", &new_model_provider).await?;
        }

        if new_model_provider == "none" {
            break;
        }

        let prefix = format!("tunnel.{new_model_provider}");
        cfg.init_defaults(Some(&prefix));
        cfg.mark_dirty(&prefix);
        cfg.save_dirty().await?;
        ui.heading(2, &new_model_provider);
        match prompt_fields_under(cfg, ui, &prefix, &[], &[]).await? {
            Nav::Back => continue,
            Nav::Done => break,
        }
    }
    mark_completed(cfg, Section::Tunnel).await?;
    Ok(Nav::Done)
}

async fn agents(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Agents, "Agents");
    loop {
        let existing_aliases: Vec<String> = cfg.get_map_keys("agents").unwrap_or_default();
        let mut options: Vec<SelectItem> = existing_aliases
            .iter()
            .map(|a| {
                let enabled_path = format!("agents.{a}.enabled");
                let is_active = cfg.get_prop(&enabled_path).ok().as_deref() == Some("true");
                if is_active {
                    SelectItem::with_badge(a.clone(), "[active]")
                } else {
                    SelectItem::with_badge(a.clone(), "[configured]")
                }
            })
            .collect();
        let add_new_idx = options.len();
        options.push(SelectItem::new("+ Add new"));
        let done_idx = options.len();
        options.push(SelectItem::new("Done"));

        let idx = match ui.select("Agent", &options, Some(done_idx)).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) => i,
        };

        if idx == done_idx {
            break;
        }

        let alias = if idx == add_new_idx {
            let suggestion = next_agent_alias_suggestion(&existing_aliases);
            let Some(a) = prompt_alias_name(ui, &suggestion).await? else {
                continue;
            };
            a
        } else {
            existing_aliases[idx].clone()
        };

        cfg.create_map_key("agents", &alias).ok();
        cfg.mark_dirty(&format!("agents.{alias}"));
        cfg.save_dirty().await?;
        let workspace_dir = cfg.agent_workspace_dir(&alias);
        if let Err(err) = tokio::fs::create_dir_all(&workspace_dir).await {
            ui.warn(&format!(
                "Could not create agent workspace at {}: {err}",
                workspace_dir.display()
            ));
        } else if let Err(err) =
            zeroclaw_config::schema::ensure_bootstrap_files(&workspace_dir).await
        {
            ::zeroclaw_log::record!(WARN, ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note).with_outcome(::zeroclaw_log::EventOutcome::Unknown).with_attrs(::serde_json::json!({"agent": alias, "workspace": workspace_dir.display().to_string(), "err": err.to_string()})), "bootstrap file seed failed (continuing): ");
        }
        ui.heading(2, &alias);
        let _ = prompt_agent_fields(cfg, ui, &alias).await?;
    }
    mark_completed(cfg, Section::Agents).await?;
    Ok(Nav::Done)
}

/// Generic OneTierAliasMap section walker — used by skill-bundles,
/// risk-profiles, runtime-profiles, peer-groups. Lists existing aliases,
/// lets the operator add a new one, and recurses into the alias's fields
/// via `prompt_fields_under`.
async fn one_tier_alias_section(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    section: Section,
    section_path: &str,
    select_label: &str,
) -> Result<Nav> {
    emit_section_header(ui, section, select_label);
    loop {
        let existing: Vec<String> = cfg.get_map_keys(section_path).unwrap_or_default();
        let mut options: Vec<SelectItem> = existing
            .iter()
            .map(|a| SelectItem::new(a.clone()))
            .collect();
        let add_new_idx = options.len();
        options.push(SelectItem::new("+ Add new"));
        let done_idx = options.len();
        options.push(SelectItem::new("Done"));

        let idx = match ui.select(select_label, &options, Some(done_idx)).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) => i,
        };

        if idx == done_idx {
            break;
        }

        let alias = if idx == add_new_idx {
            let suggestion = next_agent_alias_suggestion(&existing);
            let Some(a) = prompt_alias_name(ui, &suggestion).await? else {
                continue;
            };
            a
        } else {
            existing[idx].clone()
        };

        cfg.create_map_key(section_path, &alias).ok();
        let prefix = format!("{section_path}.{alias}");
        cfg.mark_dirty(&prefix);
        cfg.save_dirty().await?;
        ui.heading(2, &alias);
        let _ = prompt_fields_under(cfg, ui, &prefix, &[], &[]).await?;
    }
    mark_completed(cfg, section).await?;
    Ok(Nav::Done)
}

async fn skills(cfg: &mut Config, ui: &mut dyn OnboardUi, _flags: &Flags) -> Result<Nav> {
    emit_section_header(ui, Section::Skills, "Skills");
    let nav = prompt_fields_under(cfg, ui, "skills", &[], &[]).await?;
    if matches!(nav, Nav::Back) {
        return Ok(Nav::Back);
    }
    mark_completed(cfg, Section::Skills).await?;
    Ok(Nav::Done)
}

/// Suggest the next unused alias when the operator picks "+ Add new".
/// On a fresh install, suggests "default". With one existing alias, suggests
/// `{first}-2`. From there, increments until an unused suffix is found so
/// adding the 4th+ agent doesn't pre-fill an alias that already exists.
fn next_agent_alias_suggestion(existing: &[String]) -> String {
    if existing.is_empty() {
        return "default".to_string();
    }
    let base = existing[0].as_str();
    (2..)
        .map(|n| format!("{base}-{n}"))
        .find(|candidate| !existing.contains(candidate))
        .unwrap_or_else(|| format!("{base}-{}", existing.len() + 1))
}

/// Build the canonical schema path for a field on an agent alias entry.
/// `set_prop` / `get_prop` and the schema's `KNOWN` table always use
/// kebab-case field names (the `Configurable` derive does
/// `snake_to_kebab` at compile time), so callers passing snake-cased
/// field names (matching the Rust struct fields) need this conversion.
/// Centralized so future additions can't reintroduce the snake/kebab
/// drift bug.
fn agent_field_path(alias: &str, snake_field: &str) -> String {
    let kebab = snake_field.replace('_', "-");
    format!("agents.{alias}.{kebab}")
}

/// Walk the fields under `agents.<alias>` with prompts tailored to each
/// field's role: bool/text via the generic `prompt_field`, the system
/// prompt via `$EDITOR`, and every alias-reference field (channels,
/// model_provider, risk_profile, …) via a picker over the relevant
/// configured aliases. Rewinds with `Nav::Back`.
async fn prompt_agent_fields(cfg: &mut Config, ui: &mut dyn OnboardUi, alias: &str) -> Result<Nav> {
    let channel_aliases = available_channel_aliases(cfg);
    let provider_aliases = available_model_provider_aliases(cfg);
    // Paths must be kebab-case — the macro at
    // crates/zeroclaw-macros/src/lib.rs:366 builds get_map_keys arms with
    // snake_to_kebab field names. Snake_case here silently returns None →
    // empty Vec → CLI picker shows the wrong "no aliases" state when the
    // user has configured some.
    let risk_aliases = cfg.get_map_keys("risk-profiles").unwrap_or_default();
    let runtime_aliases = cfg.get_map_keys("runtime-profiles").unwrap_or_default();
    let skill_aliases = cfg.get_map_keys("skill-bundles").unwrap_or_default();
    let knowledge_aliases = cfg.get_map_keys("knowledge-bundles").unwrap_or_default();
    let mcp_aliases = cfg.get_map_keys("mcp-bundles").unwrap_or_default();

    let mut step: usize = 0;
    loop {
        let nav = match step {
            0 => prompt_field(cfg, ui, &agent_field_path(alias, "enabled"), None).await?,
            1 => prompt_agent_system_prompt(cfg, ui, alias).await?,
            2 => prompt_agent_alias_multi(cfg, ui, alias, "channels", &channel_aliases).await?,
            3 => {
                prompt_agent_alias_single(cfg, ui, alias, "model_provider", &provider_aliases)
                    .await?
            }
            4 => prompt_agent_alias_single(cfg, ui, alias, "risk_profile", &risk_aliases).await?,
            5 => {
                prompt_agent_alias_single(cfg, ui, alias, "runtime_profile", &runtime_aliases)
                    .await?
            }
            6 => prompt_agent_alias_multi(cfg, ui, alias, "skill_bundles", &skill_aliases).await?,
            7 => {
                prompt_agent_alias_multi(cfg, ui, alias, "knowledge_bundles", &knowledge_aliases)
                    .await?
            }
            8 => prompt_agent_alias_multi(cfg, ui, alias, "mcp_bundles", &mcp_aliases).await?,
            _ => return Ok(Nav::Done),
        };
        match nav {
            Nav::Done => step += 1,
            Nav::Back => {
                if step == 0 {
                    return Ok(Nav::Back);
                }
                step -= 1;
            }
        }
    }
}

/// Multi-line system-prompt editor backed by `$EDITOR`. Writes to
/// `agents/<alias>/workspace/AGENTS.md` — the per-agent markdown
/// surface the docstring has always pointed at. There is no
/// `agents.<alias>.system_prompt` config field; trying to write one
/// (the previous behavior) errored out with "Unknown property".
async fn prompt_agent_system_prompt(
    cfg: &Config,
    ui: &mut dyn OnboardUi,
    alias: &str,
) -> Result<Nav> {
    let workspace = cfg.agent_workspace_dir(alias);
    let agents_md = workspace.join("AGENTS.md");
    let initial = tokio::fs::read_to_string(&agents_md)
        .await
        .unwrap_or_default();
    ui.note(
        "Optional system prompt for this agent. Stored at \
         agents/<alias>/workspace/AGENTS.md and loaded into the system \
         prompt at agent start. Leave empty to skip.",
    );
    match ui.editor("AGENTS.md", &initial).await? {
        Answer::Back => Ok(Nav::Back),
        Answer::Value(new) => {
            if new != initial {
                tokio::fs::create_dir_all(&workspace)
                    .await
                    .with_context(|| {
                        format!(
                            "Failed to create per-agent workspace at {}",
                            workspace.display()
                        )
                    })?;
                tokio::fs::write(&agents_md, &new).await.with_context(|| {
                    format!("Failed to write AGENTS.md at {}", agents_md.display().to_string())
                })?;
            }
            Ok(Nav::Done)
        }
    }
}

/// Single-select alias picker. Always offers a `(none)` choice so the
/// field can be cleared. When the candidate list is empty, falls back to
/// a free-text prompt with a hint that no aliases are configured yet.
async fn prompt_agent_alias_single(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    alias: &str,
    field: &str,
    available: &[String],
) -> Result<Nav> {
    let path = agent_field_path(alias, field);
    let current_raw = cfg.get_prop(&path).ok().unwrap_or_default();
    let current = if current_raw == "<unset>" {
        String::new()
    } else {
        current_raw
    };
    let help = field_doc(cfg, &path).unwrap_or_default();
    ui.note(&help);

    if available.is_empty() {
        ui.note(&format!(
            "{help}\nNo {field} aliases configured yet. Press Enter to leave empty."
        ));
        match ui.string(field, Some(&current), None).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(new) => {
                if new != current {
                    persist(cfg, &path, &new).await?;
                }
                return Ok(Nav::Done);
            }
        }
    }

    let mut items: Vec<SelectItem> = vec![SelectItem::new("(none)")];
    for a in available {
        items.push(SelectItem::new(a.as_str()));
    }
    let current_idx = if current.is_empty() {
        Some(0)
    } else {
        available
            .iter()
            .position(|a| a == &current)
            .map(|i| i + 1)
            .or(Some(0))
    };
    match ui.select(field, &items, current_idx).await? {
        Answer::Back => Ok(Nav::Back),
        Answer::Value(0) => {
            if !current.is_empty() {
                persist(cfg, &path, "").await?;
            }
            Ok(Nav::Done)
        }
        Answer::Value(i) => {
            let chosen = &available[i - 1];
            if chosen != &current {
                persist(cfg, &path, chosen).await?;
            }
            Ok(Nav::Done)
        }
    }
}

/// Multi-select alias picker rendered as a vertical toggle list. Each
/// available alias is one row prefixed with `[x]` / `[ ]` and a
/// `selected` badge when chosen; selecting a row toggles its membership.
/// A trailing `Done` row commits the set. Mirrors the model_providers picker
/// (see `model_providers()` in this file) so CLI and TUI feel identical.
///
/// Empty candidate list → no-op skip. Persists nothing if the user
/// hasn't changed the selection from what's on disk.
async fn prompt_agent_alias_multi(
    cfg: &mut Config,
    ui: &mut dyn OnboardUi,
    alias: &str,
    field: &str,
    available: &[String],
) -> Result<Nav> {
    let path = agent_field_path(alias, field);
    let current_raw = cfg.get_prop(&path).ok().unwrap_or_default();
    let initial = parse_string_array_display(&current_raw);
    // Drop currently-selected entries that no longer exist as candidates;
    // the validator catches them otherwise but the picker shouldn't
    // pretend they're present.
    let mut selected: Vec<String> = initial
        .iter()
        .filter(|s| available.iter().any(|a| a == *s))
        .cloned()
        .collect();
    let help = field_doc(cfg, &path).unwrap_or_default();

    if available.is_empty() {
        ui.note(&format!(
            "{help}\nNo {field} aliases configured yet — skipping."
        ));
        return Ok(Nav::Done);
    }

    loop {
        ui.note(&format!(
            "{help}\nEnter toggles a row. Pick `Done` to commit. ({} of {} selected)",
            selected.len(),
            available.len(),
        ));

        let mut items: Vec<SelectItem> = available
            .iter()
            .map(|a| {
                let is_selected = selected.contains(a);
                let label = format!("[{}] {a}", if is_selected { "x" } else { " " });
                if is_selected {
                    SelectItem::with_badge(label, "selected")
                } else {
                    SelectItem::new(label)
                }
            })
            .collect();
        items.push(SelectItem::new("Done"));
        let done_idx = items.len() - 1;

        match ui.select(field, &items, Some(done_idx)).await? {
            Answer::Back => return Ok(Nav::Back),
            Answer::Value(i) if i == done_idx => {
                let serialized = serialize_string_array_json(&selected);
                if serialized != current_raw {
                    persist(cfg, &path, &serialized).await?;
                }
                return Ok(Nav::Done);
            }
            Answer::Value(i) => {
                let alias_at = &available[i];
                if let Some(pos) = selected.iter().position(|a| a == alias_at) {
                    selected.remove(pos);
                } else {
                    selected.push(alias_at.clone());
                }
            }
        }
    }
}

fn field_doc(cfg: &Config, path: &str) -> Option<String> {
    cfg.prop_fields()
        .into_iter()
        .find(|f| f.name == path)
        .map(|f| f.description.to_string())
}

/// Parse `get_prop`'s display form for a string array back into a Vec.
/// `get_prop` returns toml's display syntax (e.g. `["a", "b"]`), so the
/// JSON parser handles both shapes; falls back to comma-split.
fn parse_string_array_display(s: &str) -> Vec<String> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "<unset>" || trimmed == "[]" {
        return Vec::new();
    }
    if trimmed.starts_with('[')
        && let Ok(arr) = serde_json::from_str::<Vec<String>>(trimmed)
    {
        return arr;
    }
    trimmed
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn serialize_string_array_json(items: &[String]) -> String {
    serde_json::to_string(items).unwrap_or_else(|_| "[]".to_string())
}

/// All currently-configured channel aliases in dotted form
/// (`telegram.default`, `discord.work`). Pulled from `prop_fields` so it
/// reflects whatever the user has just configured.
fn available_channel_aliases(cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in cfg.prop_fields() {
        if let Some(rest) = f.name.strip_prefix("channels.") {
            let mut parts = rest.splitn(3, '.');
            if let (Some(ty), Some(alias), Some(_leaf)) = (parts.next(), parts.next(), parts.next())
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

/// All currently-configured model provider aliases in dotted form
/// (`anthropic.default`, `openrouter.work`). Pulled from `prop_fields`.
fn available_model_provider_aliases(cfg: &Config) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in cfg.prop_fields() {
        if let Some(rest) = f.name.strip_prefix("providers.models.") {
            let mut parts = rest.splitn(3, '.');
            if let (Some(ty), Some(alias), Some(_leaf)) = (parts.next(), parts.next(), parts.next())
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
    use zeroclaw_config::schema::{AnthropicModelProviderConfig, Config, ModelProviderConfig};

    #[test]
    fn next_agent_alias_suggestion_handles_empty_collision_and_growth() {
        // Fresh install → "default".
        assert_eq!(next_agent_alias_suggestion(&[]), "default");

        // Single existing → first numeric suffix.
        let one = vec!["assistant".to_string()];
        assert_eq!(next_agent_alias_suggestion(&one), "assistant-2");

        // Adding a third when -2 already exists must not collide.
        let two = vec!["assistant".to_string(), "assistant-2".to_string()];
        assert_eq!(next_agent_alias_suggestion(&two), "assistant-3");

        // Non-sequential history still finds the next gap-free suffix.
        let four = vec![
            "researcher".to_string(),
            "researcher-2".to_string(),
            "researcher-3".to_string(),
            "researcher-5".to_string(),
        ];
        assert_eq!(next_agent_alias_suggestion(&four), "researcher-4");
    }

    /// Build a `Config` whose `config_path` / `workspace_dir` live inside a
    /// temp directory, so `save()` touches only the scratch tree.
    fn test_cfg(temp: &TempDir) -> Config {
        Config {
            config_path: temp.path().join("config.toml"),
            data_dir: temp.path().join("data"),
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
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn section_has_signal_providers_requires_models_entry() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        assert!(!section_has_signal(&cfg, Section::ModelProviders));
        cfg.providers
            .models
            .ensure("anthropic", "default")
            .expect("anthropic typed slot");
        assert!(section_has_signal(&cfg, Section::ModelProviders));
    }

    #[tokio::test]
    async fn section_has_signal_hardware_tracks_enabled_flag() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        assert!(!section_has_signal(&cfg, Section::Hardware));
        cfg.hardware.enabled = true;
        assert!(section_has_signal(&cfg, Section::Hardware));
    }

    #[tokio::test]
    async fn section_has_signal_memory_and_tunnel_are_marker_only() {
        let temp = TempDir::new().unwrap();
        let cfg = test_cfg(&temp);
        // Memory defaults to "sqlite" and Tunnel defaults to "none" — both
        // are valid user choices indistinguishable from untouched defaults,
        // so the completed-sections marker is the only skip-gate signal.
        assert!(!section_has_signal(&cfg, Section::Memory));
        assert!(!section_has_signal(&cfg, Section::Tunnel));
    }

    #[tokio::test]
    async fn mark_completed_is_dedupe_safe() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        mark_completed(&mut cfg, Section::Memory).await.unwrap();
        mark_completed(&mut cfg, Section::Memory).await.unwrap();
        let count = cfg
            .onboard_state
            .completed_sections
            .iter()
            .filter(|s| s.as_str() == "memory")
            .count();
        assert_eq!(count, 1, "marker should be inserted at most once");
    }

    #[tokio::test]
    async fn skip_gate_skips_when_marked_and_user_declines() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.onboard_state.completed_sections.push("memory".into());

        // QuickUi with no scripted answers returns `default` from `confirm`,
        // which for the reconfigure prompt is `false` → SkipNav::Skip.
        let mut ui = QuickUi::new();
        let result = skip_if_configured(
            &cfg,
            &mut ui,
            &Flags::default(),
            Section::Memory,
            "Memory",
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
            Section::Memory,
            "Memory",
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
        cfg.onboard_state.completed_sections.push("memory".into());

        let mut ui = QuickUi::new();
        let flags = Flags {
            force: true,
            ..Default::default()
        };
        let result = skip_if_configured(&cfg, &mut ui, &flags, Section::Memory, "Memory", true)
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
            Section::Memory,
            "Memory",
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

        let flags = Flags::default();
        let mut ui = QuickUi::new()
            .with("ModelProvider", CUSTOM_OPENAI_COMPAT_LABEL)
            .with("OpenAI-compatible base URL", &base_url)
            .with("alias", "default")
            .with("api-key", "sk-custom-test")
            .with("Model", "qwen-local");

        Box::pin(run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &flags,
        ))
        .await
        .unwrap();

        let model_cfg = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("custom model_provider entry should be seeded");
        assert_eq!(model_cfg.api_key.as_deref(), Some("sk-custom-test"));
        assert_eq!(model_cfg.uri.as_deref(), Some(base_url.as_str()));
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
        let entry = cfg
            .providers
            .models
            .ensure("custom", "default")
            .expect("custom typed slot");
        entry.api_key = Some("sk-gateway-test".into());
        entry.uri = Some(base_url);
        let mut ui = QuickUi::new().with("Model", "gateway-large");

        prompt_model(&mut cfg, &mut ui, "providers.models.custom.default")
            .await
            .unwrap();

        let model_cfg = cfg
            .providers
            .models
            .find("custom", "default")
            .expect("custom model_provider entry should remain configured");
        assert_eq!(model_cfg.model.as_deref(), Some("gateway-large"));
    }

    /// Providers section driven entirely by CLI flags: the `--model-provider`,
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
            model_provider: Some("anthropic".into()),
            api_key: Some("sk-ant-test".into()),
            model: Some("claude-opus-4-7".into()),
            ..Default::default()
        };
        let mut ui = QuickUi::new();
        Box::pin(run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &flags,
        ))
        .await
        .unwrap();

        let model_cfg = cfg
            .providers
            .models
            .find("anthropic", "default")
            .expect("anthropic.default entry should be seeded");
        assert_eq!(model_cfg.model.as_deref(), Some("claude-opus-4-7"));
        assert_eq!(model_cfg.api_key.as_deref(), Some("sk-ant-test"));
        assert!(
            cfg.onboard_state
                .completed_sections
                .iter()
                .any(|s| s == "providers.models"),
            "providers.models section should mark completed"
        );
    }

    /// Double-run idempotency for model_providers: prime via flags, then a
    /// flags-free second run hits the skip-gate (marker + fallback +
    /// models entry = has_signal) and QuickUi's default-false confirm
    /// declines reconfigure, leaving the on-disk config byte-identical.
    #[tokio::test]
    async fn providers_second_run_no_flags_is_idempotent_on_disk() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);

        let prime = Flags {
            model_provider: Some("anthropic".into()),
            api_key: Some("sk-ant-test".into()),
            model: Some("claude-opus-4-7".into()),
            ..Default::default()
        };
        let mut ui = QuickUi::new();
        Box::pin(run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &prime,
        ))
        .await
        .unwrap();
        let after_first = tokio::fs::read_to_string(&cfg.config_path).await.unwrap();

        let mut ui = QuickUi::new();
        run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &Flags::default(),
        )
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
        Box::pin(run(&mut cfg, &mut ui, Some(Section::Channels), &flags))
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
        Box::pin(run(&mut cfg, &mut ui, Some(Section::Channels), &flags))
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
            // Vec<String> with #[serde(default)]; empty answer keeps the
            // default empty list. Same shape as proxy-url above.
            .with("excluded-tools", "")
            .with_sequence("Channel", ["telegram", "Done"]);
        Box::pin(run(&mut cfg, &mut ui, Some(Section::Channels), &flags))
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

    /// Smoke test: picking Mochat walks the config fields and the
    /// resulting config has the scripted base URL and API token
    /// round-tripped via `set_prop`.
    #[tokio::test]
    async fn channels_mochat_selection_persists_url_and_token() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let flags = Flags::default();

        let mut ui = QuickUi::new()
            .with("api-url", "http://mochat-test:8080/v1")
            .with("api-token", "stub-mochat-token")
            // Vec<String> with #[serde(default)]; empty answer keeps the
            // default empty list.
            .with("excluded-tools", "")
            .with_sequence("Channel", ["mochat", "Done"]);
        Box::pin(run(&mut cfg, &mut ui, Some(Section::Channels), &flags))
            .await
            .unwrap();

        let mc = cfg
            .channels
            .mochat
            .get("default")
            .expect("mochat subsection should be initialized");
        assert_eq!(mc.api_url, "http://mochat-test:8080/v1");
        assert_eq!(mc.api_token, "stub-mochat-token");
    }

    // ---------------------------------------------------------------------------
    // BackAt: a test-only OnboardUi that returns Answer::Back for one named
    // prompt, and delegates everything else to an inner QuickUi. Used to drive
    // ESC / Back navigation through model_provider and channel flows without spinning
    // up the full TUI.
    // ---------------------------------------------------------------------------
    struct BackAt {
        back_prompt: &'static str,
        inner: QuickUi,
    }

    impl BackAt {
        fn new(back_prompt: &'static str, inner: QuickUi) -> Self {
            Self { back_prompt, inner }
        }
    }

    #[async_trait::async_trait]
    impl OnboardUi for BackAt {
        async fn confirm(&mut self, prompt: &str, default: bool) -> anyhow::Result<Answer<bool>> {
            if prompt == self.back_prompt {
                return Ok(Answer::Back);
            }
            self.inner.confirm(prompt, default).await
        }

        async fn string(
            &mut self,
            prompt: &str,
            current: Option<&str>,
            placeholder: Option<&str>,
        ) -> anyhow::Result<Answer<String>> {
            if prompt == self.back_prompt {
                return Ok(Answer::Back);
            }
            self.inner.string(prompt, current, placeholder).await
        }

        async fn secret(
            &mut self,
            prompt: &str,
            has_current: bool,
        ) -> anyhow::Result<Answer<Option<String>>> {
            if prompt == self.back_prompt {
                return Ok(Answer::Back);
            }
            self.inner.secret(prompt, has_current).await
        }

        async fn select(
            &mut self,
            prompt: &str,
            items: &[SelectItem],
            current: Option<usize>,
        ) -> anyhow::Result<Answer<usize>> {
            if prompt == self.back_prompt {
                return Ok(Answer::Back);
            }
            self.inner.select(prompt, items, current).await
        }

        async fn editor(&mut self, hint: &str, initial: &str) -> anyhow::Result<Answer<String>> {
            if hint == self.back_prompt {
                return Ok(Answer::Back);
            }
            self.inner.editor(hint, initial).await
        }

        fn heading(&mut self, level: u8, text: &str) {
            self.inner.heading(level, text);
        }

        fn note(&mut self, msg: &str) {
            self.inner.note(msg);
        }

        fn status(&mut self, msg: &str) {
            self.inner.status(msg);
        }

        fn warn(&mut self, msg: &str) {
            self.inner.warn(msg);
        }
    }

    // US-7 / prompt_model regression: model is written to the alias the user
    // actually selected, not to a hardcoded ".default." path. A non-default
    // alias ("work") must produce model_providers.anthropic.work.model, never
    // model_providers.anthropic.default.model.
    #[tokio::test]
    async fn prompt_model_writes_to_actual_alias_not_hardcoded_default() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.providers
            .models
            .anthropic
            .insert("work".into(), AnthropicModelProviderConfig::default());

        let mut ui = QuickUi::new().with("Model", "claude-opus-4-7");
        prompt_model(&mut cfg, &mut ui, "providers.models.anthropic.work")
            .await
            .unwrap();

        let work_model = cfg
            .providers
            .models
            .find("anthropic", "work")
            .and_then(|c| c.model.as_deref());
        assert_eq!(
            work_model,
            Some("claude-opus-4-7"),
            "model must be written to the 'work' alias, not 'default'"
        );

        let default_model = cfg
            .providers
            .models
            .find("anthropic", "default")
            .and_then(|c| c.model.as_deref());
        assert!(
            default_model.is_none(),
            "no 'default' alias should exist — path was hardcoded to 'default' (regression)"
        );
    }

    // US-3 / ESC regression: backing out of api-key prompt on an existing alias
    // must leave that alias intact with its original values.
    #[tokio::test]
    async fn providers_esc_on_existing_alias_leaves_config_untouched() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);

        // Pre-seed an existing alias with a known api_key.
        cfg.providers.models.anthropic.insert(
            "my-alias".to_string(),
            AnthropicModelProviderConfig {
                base: ModelProviderConfig {
                    api_key: Some("sk-original".into()),
                    model: Some("claude-opus-4-7".into()),
                    ..Default::default()
                },
            },
        );

        // Drive model_providers(): pick anthropic, pick "my-alias" (existing), ESC at api-key.
        // The loop should continue (not remove the entry) because is_new_entry = false.
        // After ESC the loop re-presents the model_provider select — we then pick "Done".
        let mut ui = BackAt::new(
            "api-key",
            QuickUi::new()
                .with_sequence("ModelProvider", ["Anthropic", "Done"])
                .with("Alias", "my-alias"),
        );
        run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &Flags::default(),
        )
        .await
        .unwrap();

        let alias_cfg = cfg
            .providers
            .models
            .find("anthropic", "my-alias")
            .expect("my-alias must survive ESC on an existing entry");
        assert_eq!(
            alias_cfg.api_key.as_deref(),
            Some("sk-original"),
            "original api_key must not be clobbered after ESC"
        );
        assert_eq!(alias_cfg.model.as_deref(), Some("claude-opus-4-7"));
    }

    // US-1 / ESC regression: backing out of api-key prompt on a brand-new alias
    // must remove the in-progress entry so it never reaches disk.
    #[tokio::test]
    async fn providers_esc_on_new_alias_removes_entry() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);

        // No pre-existing aliases for anthropic. The alias prompt fires first,
        // user types "fresh". ESC at api-key removes "fresh" and loops. Then
        // the model_provider select fires again and the user picks "Done".
        let mut ui = BackAt::new(
            "api-key",
            QuickUi::new()
                .with_sequence("ModelProvider", ["Anthropic", "Done"])
                .with("Alias (name for this configuration)", "fresh"),
        );
        run(
            &mut cfg,
            &mut ui,
            Some(Section::ModelProviders),
            &Flags::default(),
        )
        .await
        .unwrap();

        let entry = cfg.providers.models.find("anthropic", "fresh");
        assert!(
            entry.is_none(),
            "in-progress 'fresh' alias must be removed after ESC (never persisted)"
        );
    }

    // Alias key validation — backend enforcement via create_map_key. These tests
    // exercise the generated macro code path that calls validate_alias_key before
    // inserting, so invalid aliases can never reach the config HashMap.

    #[test]
    fn create_map_key_rejects_alias_with_dot() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let result = cfg.create_map_key("channels.discord", "my.alias");
        assert!(result.is_err(), "dot in alias must be rejected");
        assert!(
            cfg.channels.discord.is_empty(),
            "no entry should be inserted"
        );
    }

    #[test]
    fn create_map_key_rejects_alias_with_slash() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let result = cfg.create_map_key("channels.discord", "prod/main");
        assert!(result.is_err(), "slash in alias must be rejected");
    }

    #[test]
    fn create_map_key_rejects_alias_with_space() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let result = cfg.create_map_key("channels.discord", "my alias");
        assert!(result.is_err(), "space in alias must be rejected");
    }

    #[test]
    fn create_map_key_rejects_alias_starting_with_hyphen() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        let result = cfg.create_map_key("channels.discord", "-bad");
        assert!(result.is_err(), "leading hyphen in alias must be rejected");
    }

    #[test]
    fn create_map_key_accepts_valid_alias() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        // V0.8.0: aliases must be lowercase ASCII alphanumeric only — see
        // `validate_alias_key`.
        let result = cfg.create_map_key("channels.discord", "prodalerts");
        assert!(result.is_ok(), "valid alias must be accepted");
        assert!(cfg.channels.discord.contains_key("prodalerts"));
    }

    #[test]
    fn create_map_key_rejects_invalid_on_providers_double_nested() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        // The typed `anthropic` slot is already declared on `ModelProviders`;
        // no insert needed to access it. The test below verifies that a dotted
        // alias key is rejected by `create_map_key`.
        // Now try to add an alias with a dot in the name.
        let result = cfg.create_map_key("providers.models.anthropic", "my.alias");
        assert!(
            result.is_err(),
            "dot in double-nested alias must be rejected"
        );
        assert!(
            cfg.providers.models.find("anthropic", "my.alias").is_none(),
            "no entry should be inserted into the inner map"
        );
    }

    // US-2: get_map_keys returns all configured aliases, not just "default".
    // Covers the gateway endpoint regression where MapKeyQuery required `key`
    // and returned 400 on every alias-list fetch.
    #[tokio::test]
    async fn get_map_keys_returns_all_channel_aliases() {
        use zeroclaw_config::schema::DiscordConfig;

        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.channels
            .discord
            .insert("default".into(), DiscordConfig::default());
        cfg.channels
            .discord
            .insert("alerts".into(), DiscordConfig::default());

        let mut keys = cfg
            .get_map_keys("channels.discord")
            .expect("discord has two entries — get_map_keys must return Some");
        keys.sort();
        assert_eq!(keys, vec!["alerts", "default"]);
    }

    // US-2 / model model_providers: get_map_keys returns all model_provider aliases across
    // both the type and alias layers.
    #[tokio::test]
    async fn get_map_keys_returns_all_provider_aliases() {
        let temp = TempDir::new().unwrap();
        let mut cfg = test_cfg(&temp);
        cfg.providers
            .models
            .anthropic
            .insert("default".into(), AnthropicModelProviderConfig::default());
        cfg.providers
            .models
            .anthropic
            .insert("work".into(), AnthropicModelProviderConfig::default());

        let mut keys = cfg
            .get_map_keys("providers.models.anthropic")
            .expect("anthropic has two aliases — get_map_keys must return Some");
        keys.sort();
        assert_eq!(keys, vec!["default", "work"]);
    }
}
