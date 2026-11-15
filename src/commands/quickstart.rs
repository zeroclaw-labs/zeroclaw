//! `zeroclaw quickstart` CLI. Extracted from main.rs.

use crate::config::Config;
use crate::gateway_helpers::{t, ta};

#[cfg(feature = "agent-runtime")]
fn qta(key: &str, args: &[(&str, &str)]) -> String {
    zeroclaw_runtime::i18n::get_required_cli_string_with_args(key, args)
}

#[cfg(feature = "agent-runtime")]
fn quickstart_row(key: &str, glyph: &str, summary: &str) -> String {
    qta(key, &[("glyph", glyph), ("summary", summary)])
}

#[cfg(feature = "agent-runtime")]
fn quickstart_step_label(step: zeroclaw_runtime::quickstart::QuickstartStep) -> String {
    t(step.label_key(), step.label())
}

#[cfg(feature = "agent-runtime")]
pub(crate) fn quickstart_runtime_profile_for_provider(
    provider_type: &str,
    providers: &[zeroclaw_runtime::quickstart::QuickstartTypeOption],
    default_runtime_profile: &str,
) -> String {
    providers
        .iter()
        .find(|provider| provider.kind == provider_type)
        .and_then(|provider| provider.default_runtime_profile.as_deref())
        .unwrap_or(default_runtime_profile)
        .to_string()
}

/// `zeroclaw quickstart` CLI entry — checklist UX, not a wizard.
///
/// Mirrors the TUI Quickstart pane's structure: a single screen
/// listing all six selectors with `[ ]` / `[✓]` status and a one-line
/// summary, the user picks which selector to fill (any order), each
/// selector opens its own picker / field-form / channel-list sub-flow,
/// and `c` creates the agent once every selector is `[✓]`. There are
/// no pre-checked defaults anywhere — every selector starts `[ ]` and
/// is only satisfied by an explicit user choice (either a "Use
/// existing" pick of an already-configured alias, or a fully-filled
/// "Create new" entry).
///
/// All option lists, field shapes, presets, and the apply path come
/// directly from `zeroclaw_runtime::quickstart` — the same module the
/// gateway and TUI surfaces consume. No RPC, no daemon: the CLI is
/// compiled in-process with `zeroclaw-runtime` and calls
/// `snapshot_state` / `field_shape` / `apply_with_surface` as plain
/// functions.
///
/// Flag pre-fills (`--model-provider`, `--model`, `--api-key`,
/// `--agent`) silently seed the relevant selector's value and mark it
/// `[✓]` if the seed is enough to satisfy the selector; the user can
/// still open that selector and overwrite it.
#[cfg(feature = "agent-runtime")]
pub async fn run_quickstart_cli(
    model_provider: Option<String>,
    model: Option<String>,
    api_key: Option<String>,
    agent: Option<String>,
) -> anyhow::Result<()> {
    use dialoguer::{Confirm, Editor, FuzzySelect, Input, Password};
    use zeroclaw_config::presets::{
        AgentIdentity, BuilderSubmission, ChannelQuickStart, MemoryChoice, ModelProviderChoice,
        SelectorChoice,
    };
    use zeroclaw_runtime::quickstart::presets::RISK_PRESETS;
    use zeroclaw_runtime::quickstart::{
        FieldSection, QuickstartTypeOption, Surface, apply_with_surface, field_shape,
        snapshot_state,
    };

    if !std::io::IsTerminal::is_terminal(&std::io::stdin())
        || !std::io::IsTerminal::is_terminal(&std::io::stderr())
    {
        anyhow::bail!(
            "{}",
            t(
                "cli-quickstart-needs-tty",
                "Quickstart is interactive and needs a terminal on stdin and stderr. \
                 Run it from an interactive shell, or use \
                 `zeroclaw config set <path> <value>` for headless configuration."
            )
        );
    }

    #[derive(Default)]
    struct Form {
        provider: Option<ProviderChoice>,
        risk: Option<PresetChoice>,
        memory: Option<MemoryChoice>,
        channels: Vec<ChannelChoice>,
        // Tracks whether the user explicitly visited Channels and
        // confirmed "no channels". An empty `channels` Vec with
        // `channels_visited == false` is *not* satisfied — the
        // selector still shows `[ ]`.
        channels_visited: bool,
        peer_groups: Vec<zeroclaw_config::presets::QuickstartPeerGroup>,
        // Mirrors `channels_visited`: peer groups are optional, so an
        // empty `peer_groups` Vec only counts as satisfied once the
        // user has actually opened the selector and left it. Until
        // then the row stays `[ ]` rather than a pre-checked default.
        peer_groups_visited: bool,
        agent: Option<AgentChoice>,
    }
    enum ProviderChoice {
        Fresh {
            kind: String,
            display_name: String,
            alias: String,
            model: String,
            /// Round-trip of every non-`model` descriptor value the
            /// daemon's `field_shape()` emitted, keyed by descriptor
            /// key. The CLI doesn't know what these mean — the daemon
            /// authored them and consumes them on the way back.
            fields: std::collections::HashMap<String, String>,
        },
        Existing {
            alias_ref: String,
        },
    }
    enum PresetChoice {
        Fresh(&'static str),
        Existing(String),
    }
    enum ChannelChoice {
        Fresh {
            kind: String,
            display_name: String,
            alias: String,
            extras: std::collections::BTreeMap<String, String>,
        },
        Existing {
            alias_ref: String,
        },
    }
    struct AgentChoice {
        name: String,
        system_prompt: String,
        personality_files: Vec<zeroclaw_config::presets::QuickstartPersonalityFile>,
    }

    impl Form {
        fn provider_done(&self) -> bool {
            self.provider.is_some()
        }
        fn risk_done(&self) -> bool {
            self.risk.is_some()
        }
        fn memory_done(&self) -> bool {
            self.memory.is_some()
        }
        fn channels_done(&self) -> bool {
            self.channels_visited
        }
        fn peer_groups_done(&self) -> bool {
            self.peer_groups_visited
        }
        fn agent_done(&self) -> bool {
            self.agent
                .as_ref()
                .is_some_and(|a| !a.name.trim().is_empty())
        }
        fn all_done(&self) -> bool {
            self.provider_done()
                && self.risk_done()
                && self.memory_done()
                && self.channels_done()
                && self.agent_done()
        }
    }

    // ── Load config + canonical registries ──────────────────────
    let _dirs = crate::config::schema::resolve_runtime_dirs().await?;
    let mut cfg = Box::pin(Config::load_or_init()).await?;
    let state = snapshot_state(&cfg);
    let providers: &[QuickstartTypeOption] = &state.model_provider_types;
    let channel_types: &[QuickstartTypeOption] = &state.channel_types;
    if providers.is_empty() {
        anyhow::bail!(
            "Quickstart could not enumerate model providers — \
             zeroclaw_providers::list_model_providers() returned no entries."
        );
    }

    let mut form = Form::default();

    if let (Some(mp), Some(m)) = (model_provider.as_deref(), model.as_deref())
        && let Some((canonical_provider, codex_auth)) =
            zeroclaw_runtime::quickstart::resolve_model_provider_type(mp)
        && let Some(found) = providers
            .iter()
            .find(|p| p.kind.eq_ignore_ascii_case(canonical_provider))
    {
        let needs_key = !found.local && api_key.is_none() && !codex_auth;
        if !needs_key {
            let mut fields: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();
            if codex_auth {
                fields.insert("auth_mode".to_string(), "codex".to_string());
            }
            if let Some(key) = api_key.as_deref().filter(|s| !s.is_empty()) {
                // Submission field keys are snake_case (`api_key`) — the apply
                // path round-trips them verbatim into `set_prop_persistent`,
                // which rejects kebab-case with "Unknown property".
                fields.insert("api_key".to_string(), key.to_string());
            }
            form.provider = Some(ProviderChoice::Fresh {
                kind: found.kind.clone(),
                display_name: found.display_name.clone(),
                alias: "default".to_string(),
                model: m.to_string(),
                fields,
            });
        }
    }
    if let Some(a) = agent.as_deref() {
        let trimmed = a.trim();
        if !trimmed.is_empty() {
            form.agent = Some(AgentChoice {
                name: trimmed.to_string(),
                system_prompt: String::new(),
                personality_files: Vec::new(),
            });
        }
    }

    // ── Main checklist loop ─────────────────────────────────────
    #[derive(Clone, Copy)]
    enum Action {
        Provider,
        Risk,
        Memory,
        Channels,
        PeerGroups,
        Agent,
        Create,
        Quit,
    }

    println!();
    println!(
        "{}",
        t(
            "cli-quickstart-title",
            "Quickstart — create one working agent end-to-end."
        )
    );
    println!();

    loop {
        // Render selector list with current status / summary.
        let glyph = |ok: bool| if ok { "[✓]" } else { "[ ]" };
        let provider_summary = match &form.provider {
            None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
            Some(ProviderChoice::Fresh {
                display_name,
                alias,
                model,
                ..
            }) => qta(
                "cli-quickstart-summary-provider-fresh",
                &[("name", display_name), ("alias", alias), ("model", model)],
            ),
            Some(ProviderChoice::Existing { alias_ref }) => qta(
                "cli-quickstart-summary-use-existing",
                &[("reference", alias_ref)],
            ),
        };
        let preset_summary = |p: &Option<PresetChoice>| -> String {
            match p {
                None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
                Some(PresetChoice::Fresh(name)) => {
                    qta("cli-quickstart-summary-preset-fresh", &[("name", name)])
                }
                Some(PresetChoice::Existing(a)) => {
                    qta("cli-quickstart-summary-use-existing", &[("reference", a)])
                }
            }
        };
        let memory_summary = match &form.memory {
            None => t("cli-quickstart-summary-not-yet-chosen", "not yet chosen"),
            Some(kind) => serde_json::to_value(kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_else(|| format!("{kind:?}").to_lowercase()),
        };
        let channels_summary = if !form.channels_visited {
            t("cli-quickstart-summary-not-yet-visited", "not yet visited")
        } else if form.channels.is_empty() {
            t(
                "cli-quickstart-summary-channels-none",
                "none (chat via `zeroclaw agent` only)",
            )
        } else {
            form.channels
                .iter()
                .map(|c| match c {
                    ChannelChoice::Fresh { kind, alias, .. } => format!("{kind}.{alias}"),
                    ChannelChoice::Existing { alias_ref } => alias_ref.clone(),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        let agent_summary = match &form.agent {
            None => t("cli-quickstart-summary-not-yet-named", "not yet named"),
            Some(a) => qta(
                "cli-quickstart-summary-agent",
                &[
                    ("alias", &a.name),
                    ("chars", &a.system_prompt.len().to_string()),
                    ("files", &a.personality_files.len().to_string()),
                ],
            ),
        };
        let peer_groups_summary = if form.peer_groups.is_empty() {
            t(
                "cli-quickstart-summary-peer-groups-none",
                "none — channels accept no peers",
            )
        } else {
            form.peer_groups
                .iter()
                .map(|pg| format!("{} → {}", pg.channel, pg.name))
                .collect::<Vec<_>>()
                .join(", ")
        };

        let risk_summary = preset_summary(&form.risk);
        let mut labels: Vec<String> = vec![
            quickstart_row(
                "cli-quickstart-row-model-provider",
                glyph(form.provider_done()),
                &provider_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-risk-profile",
                glyph(form.risk_done()),
                &risk_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-memory",
                glyph(form.memory_done()),
                &memory_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-channels",
                glyph(form.channels_done()),
                &channels_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-peer-groups",
                glyph(form.peer_groups_done()),
                &peer_groups_summary,
            ),
            quickstart_row(
                "cli-quickstart-row-agent-identity",
                glyph(form.agent_done()),
                &agent_summary,
            ),
        ];
        let create_enabled = form.all_done();
        labels.push(if create_enabled {
            t("cli-quickstart-create-agent", "── Create agent")
        } else {
            t(
                "cli-quickstart-create-agent-locked",
                "── Create agent (locked — fill every selector first)",
            )
        });

        let actions = [
            Action::Provider,
            Action::Risk,
            Action::Memory,
            Action::Channels,
            Action::PeerGroups,
            Action::Agent,
            Action::Create,
        ];

        let pick = FuzzySelect::new()
            .with_prompt(t(
                "cli-quickstart-open-selector-prompt",
                "Open a selector (Enter), or pick Create. Esc to quit.",
            ))
            .items(&labels)
            .default(0)
            .max_length(labels.len())
            .interact_opt()?;
        let action = match pick {
            Some(i) => actions[i],
            None => Action::Quit, // Esc on the main checklist quits.
        };

        match action {
            Action::Quit => {
                println!(
                    "{}",
                    t(
                        "cli-quickstart-cancelled",
                        "Quickstart cancelled. No config written."
                    )
                );
                return Ok(());
            }
            Action::Create => {
                if !create_enabled {
                    println!(
                        "{}",
                        t(
                            "cli-quickstart-incomplete",
                            "  Not all selectors are filled yet."
                        )
                    );
                    continue;
                }
                break;
            }
            Action::Provider => {
                // Step 1: pick Existing or Fresh, when there are
                // existing providers to choose from.
                let mut mode_labels: Vec<String> = Vec::new();
                let mut mode_kinds: Vec<&str> = Vec::new();
                if !state.model_providers.is_empty() {
                    mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
                    mode_kinds.push("existing");
                }
                mode_labels.push(t("cli-quickstart-create-new", "Create new"));
                mode_kinds.push("fresh");
                let mode = if mode_labels.len() == 1 {
                    Some(0)
                } else {
                    FuzzySelect::new()
                        .with_prompt(t("cli-quickstart-model-provider-prompt", "Model provider"))
                        .items(&mode_labels)
                        .default(0)
                        .max_length(mode_labels.len())
                        .interact_opt()?
                };
                let Some(mi) = mode else { continue };
                if mode_kinds[mi] == "existing" {
                    let labels: Vec<String> = state.model_providers.clone();
                    let Some(i) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-pick-configured-provider",
                            "Pick a configured provider",
                        ))
                        .items(&labels)
                        .default(0)
                        .max_length(labels.len().max(1))
                        .interact_opt()?
                    else {
                        continue;
                    };
                    form.provider = Some(ProviderChoice::Existing {
                        alias_ref: labels[i].clone(),
                    });
                    continue;
                }
                // Fresh: type → alias → field form.
                let prov_labels: Vec<String> = providers
                    .iter()
                    .map(|p| {
                        if p.local {
                            qta(
                                "cli-quickstart-provider-local-label",
                                &[("name", &p.display_name)],
                            )
                        } else {
                            p.display_name.clone()
                        }
                    })
                    .collect();
                let Some(pi) = FuzzySelect::new()
                    .with_prompt(t("cli-quickstart-provider-type-prompt", "Provider type"))
                    .items(&prov_labels)
                    .default(0)
                    .max_length(prov_labels.len().max(1))
                    .interact_opt()?
                else {
                    continue;
                };
                let chosen = &providers[pi];
                let Ok(alias) = Input::<String>::new()
                    .with_prompt(qta(
                        "cli-quickstart-alias-for",
                        &[("name", &chosen.display_name)],
                    ))
                    .default("default".to_string())
                    .allow_empty(false)
                    .validate_with(|input: &String| {
                        zeroclaw_config::helpers::validate_alias_key(input)
                    })
                    .interact_text()
                else {
                    continue;
                };
                // Field shape from the canonical schema.
                let descriptors = field_shape(FieldSection::ModelProvider, &chosen.kind);
                let mut model = String::new();
                let mut field_buf: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();
                let mut aborted = false;
                for d in &descriptors {
                    if d.key == "api_key" {
                        let skips_api_key = crate::commands::auth::quickstart_field_value_eq(
                            &field_buf,
                            "auth_mode",
                            "codex",
                        ) || (chosen.kind == "anthropic"
                            && crate::commands::auth::quickstart_field_value_eq(
                                &field_buf,
                                "auth_mode",
                                "setup_token",
                            ));
                        if skips_api_key {
                            continue;
                        }
                    }
                    // For the model field, upgrade the descriptor with a
                    // live catalog so `prompt_for_field` renders a picker
                    // instead of a free-text input. Empty catalog (live=false)
                    // leaves the descriptor unchanged → free-text fallback.
                    let upgraded;
                    let d_used = if d.key.eq_ignore_ascii_case("model") {
                        let (models, _pricing, live) =
                            zeroclaw_runtime::quickstart::model_catalog(&chosen.kind).await;
                        if live && !models.is_empty() {
                            upgraded = zeroclaw_runtime::quickstart::FieldDescriptor {
                                kind: zeroclaw_api::config_metadata::PropKind::Enum,
                                enum_variants: Some(models),
                                ..d.clone()
                            };
                            &upgraded
                        } else {
                            d
                        }
                    } else {
                        d
                    };
                    let collected = prompt_for_field(d_used, None)?;
                    let Some(value) = collected else {
                        aborted = true;
                        break;
                    };
                    // `model` is hoisted to a top-level field on
                    // ProviderChoice for the summary line. Every other
                    // descriptor flows through `field_buf` keyed by
                    // its schema identifier — no cherry-picking.
                    if d.key.eq_ignore_ascii_case("model") {
                        model = value;
                    } else if !value.is_empty() && value != zeroclaw_config::traits::UNSET_DISPLAY {
                        field_buf.insert(d.key.clone(), value);
                    }
                }
                if aborted {
                    continue;
                }
                if model.is_empty() {
                    eprintln!(
                        "{}",
                        qta(
                            "cli-quickstart-model-field-missing-warning",
                            &[("provider", &chosen.kind)],
                        )
                    );
                    let Ok(m) = Input::<String>::new()
                        .with_prompt(qta(
                            "cli-quickstart-model-id-for",
                            &[("name", &chosen.display_name)],
                        ))
                        .allow_empty(false)
                        .interact_text()
                    else {
                        continue;
                    };
                    model = m;
                }
                form.provider = Some(ProviderChoice::Fresh {
                    kind: chosen.kind.clone(),
                    display_name: chosen.display_name.clone(),
                    alias,
                    model,
                    fields: field_buf,
                });
            }
            Action::Risk => {
                let chosen = pick_preset(
                    &t("cli-quickstart-risk-profile-prompt", "Risk profile"),
                    RISK_PRESETS
                        .iter()
                        .map(|p| (p.preset_name, p.label, p.help))
                        .collect(),
                    &state.risk_profiles,
                )?;
                if let Some(c) = chosen {
                    form.risk = Some(match c {
                        Ok(name) => PresetChoice::Fresh(name),
                        Err(alias) => PresetChoice::Existing(alias),
                    });
                }
            }
            Action::Memory => {
                let kinds: [MemoryChoice; 4] = [
                    MemoryChoice::Sqlite,
                    MemoryChoice::Markdown,
                    MemoryChoice::Lucid,
                    MemoryChoice::None,
                ];
                // The retired postgres/qdrant variants only parse for old
                // configs and are never offered.
                #[allow(clippy::no_effect_underscore_binding)]
                let _exhaustive = |k: MemoryChoice| match k {
                    MemoryChoice::Sqlite
                    | MemoryChoice::Markdown
                    | MemoryChoice::Lucid
                    | MemoryChoice::Tachi
                    | MemoryChoice::None
                    | MemoryChoice::Postgres
                    | MemoryChoice::Qdrant => (),
                };
                let labels: Vec<String> = kinds
                    .iter()
                    .map(|k| {
                        serde_json::to_value(k)
                            .ok()
                            .and_then(|v| v.as_str().map(str::to_string))
                            .unwrap_or_else(|| format!("{k:?}").to_lowercase())
                    })
                    .collect();
                let Some(i) = FuzzySelect::new()
                    .with_prompt(t("cli-quickstart-memory-backend-prompt", "Memory backend"))
                    .items(&labels)
                    .default(0)
                    .max_length(labels.len().max(1))
                    .interact_opt()?
                else {
                    continue;
                };
                form.memory = Some(kinds[i]);
            }
            Action::Channels => {
                // Channels sub-flow: list current drafts + Add / Done.
                loop {
                    let mut items: Vec<String> = form
                        .channels
                        .iter()
                        .map(|c| match c {
                            ChannelChoice::Fresh { kind, alias, .. } => qta(
                                "cli-quickstart-channel-remove-row",
                                &[("reference", &format!("{kind}.{alias}"))],
                            ),
                            ChannelChoice::Existing { alias_ref } => qta(
                                "cli-quickstart-channel-remove-row",
                                &[("reference", alias_ref)],
                            ),
                        })
                        .collect();
                    items.push(t("cli-quickstart-add-channel", "+ Add a channel"));
                    items.push(t(
                        "cli-quickstart-channels-done",
                        "Done (channels selector counts as visited)",
                    ));
                    let Some(i) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-channels-prompt",
                            "Channels (optional, 0..N)",
                        ))
                        .items(&items)
                        .default(items.len().saturating_sub(2))
                        .max_length(items.len())
                        .interact_opt()?
                    else {
                        break;
                    };
                    if i < form.channels.len() {
                        form.channels.remove(i);
                        continue;
                    }
                    if i == form.channels.len() {
                        // Add — pick Existing or Fresh.
                        let mut mode_labels: Vec<String> = Vec::new();
                        let mut mode_kinds: Vec<&str> = Vec::new();
                        if !state.unassigned_channels.is_empty() {
                            mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
                            mode_kinds.push("existing");
                        }
                        mode_labels.push(t("cli-quickstart-create-new", "Create new"));
                        mode_kinds.push("fresh");
                        let mode = if mode_labels.len() == 1 {
                            Some(0)
                        } else {
                            FuzzySelect::new()
                                .with_prompt(t(
                                    "cli-quickstart-channel-source-prompt",
                                    "Channel source",
                                ))
                                .items(&mode_labels)
                                .default(0)
                                .max_length(mode_labels.len())
                                .interact_opt()?
                        };
                        let Some(mi) = mode else { continue };
                        if mode_kinds[mi] == "existing" {
                            let labels: Vec<String> = state.unassigned_channels.clone();
                            if labels.is_empty() {
                                println!(
                                    "{}",
                                    t(
                                        "cli-quickstart-all-channels-bound",
                                        "  Every configured channel is already bound to an agent. Free one with `zeroclaw config set agents.<alias>.channels ...` before reusing it here.",
                                    )
                                );
                                continue;
                            }
                            let Some(ei) = FuzzySelect::new()
                                .with_prompt(t(
                                    "cli-quickstart-pick-configured-channel",
                                    "Pick a configured channel",
                                ))
                                .items(&labels)
                                .default(0)
                                .max_length(labels.len().max(1))
                                .interact_opt()?
                            else {
                                continue;
                            };
                            form.channels.push(ChannelChoice::Existing {
                                alias_ref: labels[ei].clone(),
                            });
                            continue;
                        }
                        if channel_types.is_empty() {
                            println!(
                                "{}",
                                t(
                                    "cli-no-channels-compiled",
                                    "  No channel types are compiled into this binary."
                                )
                            );
                            continue;
                        }
                        let labels: Vec<String> = channel_types
                            .iter()
                            .map(|c| c.display_name.clone())
                            .collect();
                        let Some(ci) = FuzzySelect::new()
                            .with_prompt(t("cli-quickstart-channel-type-prompt", "Channel type"))
                            .items(&labels)
                            .default(0)
                            .max_length(labels.len().max(1))
                            .interact_opt()?
                        else {
                            continue;
                        };
                        let chosen = &channel_types[ci];
                        let Ok(alias) = Input::<String>::new()
                            .with_prompt(qta(
                                "cli-quickstart-alias-for",
                                &[("name", &chosen.display_name)],
                            ))
                            .default(chosen.kind.clone())
                            .allow_empty(false)
                            .interact_text()
                        else {
                            continue;
                        };
                        let descriptors = field_shape(FieldSection::Channel, &chosen.kind);
                        let mut extras: std::collections::BTreeMap<String, String> =
                            std::collections::BTreeMap::new();
                        let mut aborted = false;
                        for d in &descriptors {
                            let Some(value) = prompt_for_field(d, None)? else {
                                aborted = true;
                                break;
                            };
                            if !value.is_empty() && value != zeroclaw_config::traits::UNSET_DISPLAY
                            {
                                extras.insert(d.key.clone(), value);
                            }
                        }
                        if aborted {
                            continue;
                        }
                        form.channels.push(ChannelChoice::Fresh {
                            kind: chosen.kind.clone(),
                            display_name: chosen.display_name.clone(),
                            alias,
                            extras,
                        });
                        continue;
                    }
                    // Done.
                    form.channels_visited = true;
                    break;
                }
            }
            Action::PeerGroups => {
                // Available channel refs: staged channels (this run) +
                // unassigned channels already in config. Refs already
                // covered by a staged peer-group are filtered out.
                let staged_refs: Vec<String> = form
                    .channels
                    .iter()
                    .map(|c| match c {
                        ChannelChoice::Fresh { kind, alias, .. } => format!("{kind}.{alias}"),
                        ChannelChoice::Existing { alias_ref } => alias_ref.clone(),
                    })
                    .collect();
                let claimed: std::collections::HashSet<String> = form
                    .peer_groups
                    .iter()
                    .map(|pg| pg.channel.clone())
                    .collect();
                let mut available: Vec<String> = staged_refs
                    .iter()
                    .chain(state.unassigned_channels.iter())
                    .filter(|r| !claimed.contains(r.as_str()))
                    .cloned()
                    .collect();
                available.dedup();
                loop {
                    let mut items: Vec<String> = form
                        .peer_groups
                        .iter()
                        .map(|pg| {
                            qta(
                                "cli-quickstart-peer-group-row",
                                &[
                                    ("channel", &pg.channel),
                                    ("name", &pg.name),
                                    ("count", &pg.external_peers.len().to_string()),
                                ],
                            )
                        })
                        .collect();
                    let drafts = items.len();
                    if !available.is_empty() {
                        items.push(t("cli-quickstart-add-peer-group", "+ Add peer group"));
                    }
                    items.push(t("cli-quickstart-done", "Done"));
                    let Some(pick) = FuzzySelect::new()
                        .with_prompt(t(
                            "cli-quickstart-peer-groups-prompt",
                            "Peer groups (Enter on a row to remove, + Add to create)",
                        ))
                        .items(&items)
                        .default(items.len() - 1)
                        .max_length(items.len())
                        .interact_opt()?
                    else {
                        break;
                    };
                    if pick < drafts {
                        form.peer_groups.remove(pick);
                        continue;
                    }
                    if pick == drafts && !available.is_empty() {
                        let Some(ch_idx) = FuzzySelect::new()
                            .with_prompt(t(
                                "cli-quickstart-channel-to-authorize-prompt",
                                "Channel to authorize",
                            ))
                            .items(&available)
                            .default(0)
                            .max_length(available.len())
                            .interact_opt()?
                        else {
                            continue;
                        };
                        let channel = available[ch_idx].clone();
                        let (ch_type, ch_alias) = match channel.split_once('.') {
                            Some(parts) => parts,
                            None => continue,
                        };
                        let name = format!("{ch_type}_{ch_alias}_default");
                        let Ok(peers_raw) = Input::<String>::new()
                            .with_prompt(t(
                                "cli-quickstart-external-peers-prompt",
                                "External peers (comma- or newline-separated, blank for none)",
                            ))
                            .allow_empty(true)
                            .interact_text()
                        else {
                            continue;
                        };
                        let external_peers: Vec<String> = peers_raw
                            .split([',', '\n'])
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty())
                            .collect();
                        form.peer_groups
                            .push(zeroclaw_config::presets::QuickstartPeerGroup {
                                name,
                                channel,
                                external_peers,
                                ignore: Vec::new(),
                            });
                        // The channel just got claimed; refresh the available list.
                        available = staged_refs
                            .iter()
                            .chain(state.unassigned_channels.iter())
                            .filter(|r| !form.peer_groups.iter().any(|pg| &pg.channel == *r))
                            .cloned()
                            .collect();
                        available.dedup();
                        continue;
                    }
                    // Done.
                    form.peer_groups_visited = true;
                    break;
                }
            }
            Action::Agent => {
                let default_name = form
                    .agent
                    .as_ref()
                    .map(|a| a.name.clone())
                    .unwrap_or_default();
                let mut input = Input::<String>::new()
                    .with_prompt(t("cli-quickstart-agent-alias-prompt", "Agent alias"))
                    .allow_empty(false)
                    .validate_with(|input: &String| {
                        zeroclaw_config::helpers::validate_alias_key(input)
                    });
                if !default_name.is_empty() {
                    input = input.default(default_name);
                }
                let Ok(name) = input.interact_text() else {
                    continue;
                };
                let mut system_prompt = form
                    .agent
                    .as_ref()
                    .map(|a| a.system_prompt.clone())
                    .unwrap_or_default();
                let edit = Confirm::new()
                    .with_prompt(t(
                        "cli-quickstart-edit-system-prompt",
                        "Edit system prompt in $EDITOR? (blank if you skip)",
                    ))
                    .default(false)
                    .interact_opt()?;
                if let Some(true) = edit
                    && let Some(edited) = Editor::new().edit(&system_prompt)?
                {
                    system_prompt = edited;
                }
                // Personality files. The canonical list comes from the
                // snapshot — no hardcoded filenames. Pre-seed buffers
                // from any previously-staged content so re-entering
                // Agent doesn't drop the user's edits.
                let prior_files: std::collections::HashMap<String, String> = form
                    .agent
                    .as_ref()
                    .map(|a| {
                        a.personality_files
                            .iter()
                            .map(|f| (f.filename.clone(), f.content.clone()))
                            .collect()
                    })
                    .unwrap_or_default();
                // Pre-render the default template set once; the per-file
                // [t] Use template option seeds the editor from this map.
                let template_ctx =
                    zeroclaw_runtime::agent::personality_templates::TemplateContext {
                        agent: trimmed_agent_name_for_templates(
                            form.agent.as_ref().map(|a| a.name.as_str()),
                        ),
                        ..Default::default()
                    };
                let templates: std::collections::HashMap<String, String> =
                    zeroclaw_runtime::agent::personality_templates::render_preset_default(
                        &template_ctx,
                    )
                    .into_iter()
                    .map(|(filename, content)| (filename.to_string(), content))
                    .collect();
                let mut personality_results: std::collections::HashMap<String, String> =
                    std::collections::HashMap::new();

                #[derive(Clone, Copy)]
                enum PersonalityAction {
                    StartWithTemplate,
                    StartFromScratch,
                    Skip,
                }
                impl PersonalityAction {
                    fn label(self, has_staged: bool) -> String {
                        match self {
                            Self::StartWithTemplate => t(
                                "cli-quickstart-personality-start-template",
                                "Start with template (open in $EDITOR)",
                            ),
                            Self::StartFromScratch => {
                                if has_staged {
                                    t(
                                        "cli-quickstart-personality-start-current",
                                        "Start from current content (open in $EDITOR)",
                                    )
                                } else {
                                    t(
                                        "cli-quickstart-personality-start-scratch",
                                        "Start from scratch (open in $EDITOR)",
                                    )
                                }
                            }
                            Self::Skip => t("cli-quickstart-personality-skip", "Skip"),
                        }
                    }
                }

                let files = state.personality_files;
                let mut idx: usize = 0;
                let mut back_to_checklist = false;
                while idx < files.len() {
                    let filename = files[idx];
                    // Prefer a decision made earlier in this loop (e.g. after
                    // stepping back), else fall back to any pre-staged content.
                    let staged = personality_results
                        .get(filename)
                        .or_else(|| prior_files.get(filename))
                        .cloned()
                        .unwrap_or_default();
                    let template_available = templates.contains_key(filename);

                    let mut actions: Vec<PersonalityAction> = Vec::with_capacity(3);
                    if template_available {
                        actions.push(PersonalityAction::StartWithTemplate);
                    }
                    actions.push(PersonalityAction::StartFromScratch);
                    actions.push(PersonalityAction::Skip);
                    let has_staged = !staged.is_empty();
                    let choices: Vec<String> =
                        actions.iter().map(|a| a.label(has_staged)).collect();
                    let position = if files.len() > 1 {
                        format!(" [{}/{}]", idx + 1, files.len())
                    } else {
                        String::new()
                    };
                    let back_hint = if idx > 0 {
                        t("cli-quickstart-esc-go-back", " (Esc to go back)")
                    } else {
                        t(
                            "cli-quickstart-esc-return-checklist",
                            " (Esc to return to checklist)",
                        )
                    };
                    let label = qta(
                        "cli-quickstart-personality-file-prompt",
                        &[
                            ("filename", filename),
                            ("position", &position),
                            ("back_hint", &back_hint),
                        ],
                    );
                    let Some(pick) = FuzzySelect::new()
                        .with_prompt(label)
                        .items(&choices)
                        .default(0)
                        .max_length(choices.len())
                        .interact_opt()?
                    else {
                        // Esc steps back one file in the stack. On the first
                        // file there's nowhere earlier to go, so it returns to
                        // the base checklist.
                        if idx == 0 {
                            back_to_checklist = true;
                            break;
                        }
                        idx -= 1;
                        continue;
                    };
                    match actions[pick] {
                        PersonalityAction::StartWithTemplate => {
                            let seed = templates
                                .get(filename)
                                .cloned()
                                .unwrap_or_else(|| staged.clone());
                            if let Some(edited) = Editor::new().edit(&seed)?
                                && !edited.trim().is_empty()
                            {
                                personality_results.insert(filename.to_string(), edited);
                            }
                        }
                        PersonalityAction::StartFromScratch => {
                            if let Some(edited) = Editor::new().edit(&staged)?
                                && !edited.trim().is_empty()
                            {
                                personality_results.insert(filename.to_string(), edited);
                            }
                        }
                        PersonalityAction::Skip => {
                            // Keep any previously-staged content rather than
                            // dropping it silently.
                            if has_staged {
                                personality_results.insert(filename.to_string(), staged);
                            }
                        }
                    }
                    idx += 1;
                }
                if back_to_checklist {
                    continue;
                }
                // Materialize in canonical file order; only files with content.
                let personality_files: Vec<zeroclaw_config::presets::QuickstartPersonalityFile> =
                    files
                        .iter()
                        .filter_map(|filename| {
                            personality_results.get(*filename).map(|content| {
                                zeroclaw_config::presets::QuickstartPersonalityFile {
                                    filename: (*filename).to_string(),
                                    content: content.clone(),
                                }
                            })
                        })
                        .collect();
                form.agent = Some(AgentChoice {
                    name,
                    system_prompt,
                    personality_files,
                });
            }
        }
    }

    // ── Assemble submission ─────────────────────────────────────
    let inline_auth = match form.provider.as_ref() {
        Some(ProviderChoice::Fresh {
            kind,
            alias,
            fields,
            ..
        }) => crate::commands::auth::quickstart_inline_auth(kind, alias, fields),
        _ => None,
    };

    let provider = form.provider.expect("provider satisfied");
    let provider_type = match &provider {
        ProviderChoice::Fresh { kind, .. } => kind.as_str(),
        ProviderChoice::Existing { alias_ref } => alias_ref
            .split_once('.')
            .map(|(provider_type, _)| provider_type)
            .unwrap_or(alias_ref),
    };
    let runtime_profile = SelectorChoice::Fresh(quickstart_runtime_profile_for_provider(
        provider_type,
        providers,
        &state.default_runtime_profile,
    ));
    let model_provider = match provider {
        ProviderChoice::Fresh {
            kind,
            alias,
            model,
            fields,
            ..
        } => SelectorChoice::Fresh(ModelProviderChoice {
            provider_type: kind,
            alias,
            model,
            fields,
        }),
        ProviderChoice::Existing { alias_ref } => SelectorChoice::Existing(alias_ref),
    };
    let risk_profile = match form.risk.expect("risk satisfied") {
        PresetChoice::Fresh(n) => SelectorChoice::Fresh(n.to_string()),
        PresetChoice::Existing(a) => SelectorChoice::Existing(a),
    };
    let memory = SelectorChoice::Fresh(form.memory.expect("memory satisfied"));
    let channels = form
        .channels
        .into_iter()
        .map(|c| match c {
            ChannelChoice::Fresh {
                kind,
                alias,
                extras,
                ..
            } => SelectorChoice::Fresh(ChannelQuickStart {
                channel_type: kind,
                alias,
                fields: extras.into_iter().collect(),
            }),
            ChannelChoice::Existing { alias_ref } => SelectorChoice::Existing(alias_ref),
        })
        .collect();
    let agent_choice = form.agent.expect("agent satisfied");
    let submission = BuilderSubmission {
        model_provider,
        risk_profile,
        runtime_profile,
        memory,
        channels,
        peer_groups: form.peer_groups,
        agent: AgentIdentity {
            name: agent_choice.name.clone(),
            system_prompt: agent_choice.system_prompt,
            personality_file: None,
            personality_files: agent_choice.personality_files,
        },
    };

    match Box::pin(apply_with_surface(submission, &mut cfg, Surface::Cli)).await {
        Ok(applied) => {
            println!();
            println!(
                "{}",
                ta(
                    "cli-quickstart-complete",
                    &[("alias", &applied.alias)],
                    "Quickstart complete."
                )
            );
            if let Some(auth) = inline_auth {
                Box::pin(crate::commands::auth::run_inline_provider_auth(
                    auth, &mut cfg,
                ))
                .await;
            }
            println!();
            println!("{}", t("cli-next-steps", "Next steps:"));
            println!(
                "{}",
                qta(
                    "cli-quickstart-next-agent-command",
                    &[("alias", &applied.alias)]
                )
            );
            Ok(())
        }
        Err(errs) => {
            eprintln!();
            eprintln!(
                "{}",
                t(
                    "cli-agent-not-created",
                    "Your agent was not created — and nothing on disk was changed."
                )
            );
            eprintln!(
                "{}",
                t(
                    "cli-quickstart-fix-and-rerun",
                    "Your existing config is untouched. Fix the following and run quickstart again:",
                )
            );
            eprintln!();
            for e in &errs {
                eprintln!("  • {}: {}", quickstart_step_label(e.step), e.message);
            }
            eprintln!();
            anyhow::bail!(
                "{}",
                qta(
                    "cli-quickstart-could-not-finish",
                    &[("count", &errs.len().to_string())],
                )
            )
        }
    }
}

#[cfg(feature = "agent-runtime")]
fn trimmed_agent_name_for_templates(prior_name: Option<&str>) -> String {
    prior_name
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            zeroclaw_runtime::agent::personality_templates::TemplateContext::default().agent
        })
}

#[cfg(feature = "agent-runtime")]
fn prompt_for_field(
    desc: &zeroclaw_runtime::quickstart::FieldDescriptor,
    seed: Option<&str>,
) -> anyhow::Result<Option<String>> {
    use dialoguer::{FuzzySelect, Input, Password};
    use zeroclaw_api::config_metadata::PropKind;
    if !desc.help.is_empty() {
        println!("  {}", desc.help);
    }
    let prompt = desc.label.clone();
    if desc.is_secret {
        // dialoguer 0.12 has no Esc-cancellable Password — only Ctrl+C
        // (returns `ErrorKind::Interrupted` wrapped in `dialoguer::Error::IO`).
        // Map that to `Ok(None)` so the caller treats it as "user backed
        // out" instead of bubbling a confusing IO-error message.
        match Password::new()
            .with_prompt(prompt.clone())
            .allow_empty_password(true)
            .interact()
        {
            Ok(pw) => return Ok(Some(pw)),
            Err(e) => {
                let io: std::io::Error = e.into();
                if io.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(None);
                }
                return Err(io.into());
            }
        }
    }
    if let (PropKind::Enum, Some(variants)) = (&desc.kind, &desc.enum_variants) {
        let Some(i) = FuzzySelect::new()
            .with_prompt(prompt)
            .items(variants)
            .default(0)
            .max_length(variants.len().max(1))
            .interact_opt()?
        else {
            return Ok(None);
        };
        return Ok(Some(variants[i].clone()));
    }
    let mut input = Input::<String>::new()
        .with_prompt(prompt)
        .allow_empty(!desc.required);
    if let Some(s) = seed {
        input = input.default(s.to_string());
    } else if let Some(d) = desc.default.as_deref()
        && !d.is_empty()
        && d != zeroclaw_config::traits::UNSET_DISPLAY
    {
        // `<unset>` is a display placeholder for an unset Option, not a
        // real default. Seeding it pre-fills the prompt so a bare Enter
        // submits `<unset>`, which the daemon then validates against the
        // field's true type (e.g. a bool) and rejects.
        input = input.default(d.to_string());
    }
    // Same Ctrl+C-as-cancel mapping as the Password branch above.
    match input.interact_text() {
        Ok(v) => Ok(Some(v)),
        Err(e) => {
            let io: std::io::Error = e.into();
            if io.kind() == std::io::ErrorKind::Interrupted {
                Ok(None)
            } else {
                Err(io.into())
            }
        }
    }
}

#[cfg(feature = "agent-runtime")]
fn pick_preset(
    prompt: &str,
    presets: Vec<(&'static str, &'static str, &'static str)>,
    existing: &[String],
) -> anyhow::Result<Option<Result<&'static str, String>>> {
    use dialoguer::FuzzySelect;
    let mut mode_labels: Vec<String> = Vec::new();
    let mut mode_kinds: Vec<&str> = Vec::new();
    if !existing.is_empty() {
        mode_labels.push(t("cli-quickstart-use-existing", "Use existing"));
        mode_kinds.push("existing");
    }
    mode_labels.push(t("cli-quickstart-pick-preset", "Pick a preset"));
    mode_kinds.push("preset");
    let mode = if mode_labels.len() == 1 {
        Some(0)
    } else {
        FuzzySelect::new()
            .with_prompt(prompt)
            .items(&mode_labels)
            .default(0)
            .max_length(mode_labels.len())
            .interact_opt()?
    };
    let Some(mi) = mode else { return Ok(None) };
    if mode_kinds[mi] == "existing" {
        let Some(i) = FuzzySelect::new()
            .with_prompt(qta(
                "cli-quickstart-pick-existing-prompt",
                &[("prompt", prompt)],
            ))
            .items(existing)
            .default(0)
            .max_length(existing.len().max(1))
            .interact_opt()?
        else {
            return Ok(None);
        };
        return Ok(Some(Err(existing[i].clone())));
    }
    let labels: Vec<String> = presets
        .iter()
        .map(|(_, label, help)| format!("{label}  —  {help}"))
        .collect();
    let Some(i) = FuzzySelect::new()
        .with_prompt(qta(
            "cli-quickstart-pick-preset-prompt",
            &[("prompt", prompt)],
        ))
        .items(&labels)
        .default(0)
        .max_length(labels.len().max(1))
        .interact_opt()?
    else {
        return Ok(None);
    };
    Ok(Some(Ok(presets[i].0)))
}
