//! Slash-command registration + reconcile (the prototype `/ask` plus one
//! command per `slash`-tagged skill). Discord delivers application-command
//! interactions over the same Gateway WebSocket as INTERACTION_CREATE; this
//! module owns deriving the desired command set from installed skills and
//! reconciling it against Discord's REST API — idempotent upsert + stale-command
//! reaping, with persisted-fingerprint and `Retry-After` durability via
//! `discord_slash_state`. The READY-time orchestration, the dispatch arm, and
//! the interaction callbacks live in `mod.rs` / `interaction`.

use serde_json::json;

use super::slash_options::{Choice, OptKind, OptionSpec};
use super::types::{DiscordSlashCommandSpec, ReconcileOutcome, SlashScope};

/// Discord caps an application at 100 global commands; stay under it with
/// headroom for `/ask` and future built-ins.
pub(crate) const MAX_SKILL_SLASH_COMMANDS: usize = 90;

/// Squeeze a skill name into Discord's command-name charset
/// (`^[a-z0-9_-]{1,32}$`): ASCII-lowercase, runs of anything else collapse
/// to a single `-`. Deliberately stricter than Discord's full unicode
/// charset — an all-non-ASCII name slugs to empty and is dropped (with a
/// WARN naming the skill), which is a documented limitation.
pub(crate) fn discord_command_slug(name: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = true; // suppress leading '-'
    for c in name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            slug.push(c);
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
        if slug.len() == 32 {
            break;
        }
    }
    slug.trim_end_matches('-').to_string()
}

/// Map installed skills to slash-command specs. Exposure rules:
/// - opt-in via the `slash` tag — skills run shell/HTTP tools, so surfacing
///   one to a whole guild must be a deliberate per-skill decision;
/// - community-synced skills (tag `open-skills`) are excluded even when
///   tagged: their manifests are third-party-controlled, and a remote
///   commit must not be able to surface new commands (name + description
///   render in every guild's Discord UI) without operator action.
///
/// Specs are sorted by slug so the output (and everything derived from it:
/// the registration fingerprint, collision winners, the cap cutoff) is
/// deterministic regardless of filesystem iteration order. Reserved names,
/// empty slugs, and collisions are dropped with a WARN; the set caps at
/// `MAX_SKILL_SLASH_COMMANDS` with dropped names logged (no silent caps).
pub fn discord_slash_specs_from_skills(
    skills: &[zeroclaw_runtime::skills::Skill],
) -> Vec<DiscordSlashCommandSpec> {
    let mut candidates: Vec<&zeroclaw_runtime::skills::Skill> = skills
        .iter()
        .filter(|s| s.tags.iter().any(|t| t == "slash"))
        .filter(|s| !s.tags.iter().any(|t| t == "open-skills"))
        .collect();
    candidates.sort_by(|a, b| a.name.cmp(&b.name));

    let mut seen = std::collections::HashSet::new();
    seen.insert("ask".to_string());
    let mut specs = Vec::new();
    for skill in candidates {
        let slug = discord_command_slug(&skill.name);
        if slug.is_empty() || !seen.insert(slug.clone()) {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "skill": skill.name,
                        "slug": slug,
                    })),
                "skipping skill slash command (reserved, empty, or colliding slug)"
            );
            continue;
        }
        let description = if skill.description.is_empty() {
            format!("Run the {} skill", skill.name)
        } else {
            skill.description.clone()
        };
        let skill_name: String = skill
            .name
            .chars()
            .map(|c| {
                if c == '\n' || c == '\r' || c == '\'' {
                    ' '
                } else {
                    c
                }
            })
            .collect();
        specs.push(DiscordSlashCommandSpec {
            skill_name,
            slug,
            description: description.chars().take(100).collect(),
            description_localizations: valid_discord_localizations(
                &skill.description_localizations,
                &skill.name,
                "command",
            ),
            options: map_skill_slash_options(skill),
        });
    }
    specs.sort_by(|a, b| a.slug.cmp(&b.slug));
    map_options_cap(&mut specs);
    specs
}

/// Map a skill's `[[skill.slash_options]]` declarations into Discord option
/// specs. Every authoring mistake is sanitised or dropped with a WARN rather
/// than passed through to Discord, because an invalid registration body is
/// rejected with a 400 and `reconcile_slash_commands` would then retry it on
/// every READY (a re-registration loop). Specifically: an unknown `type` drops
/// the option; the name is slugged to Discord's option-name charset (dropped if
/// it slugs to empty) and de-duplicated within the command; numeric choices
/// whose value doesn't parse to the option type are dropped; inverted `min`/
/// `max` bounds are dropped. Required options are sorted first (Discord rejects
/// a required option that follows an optional one).
fn map_skill_slash_options(skill: &zeroclaw_runtime::skills::Skill) -> Vec<OptionSpec> {
    let mut options = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for decl in &skill.slash_options {
        let Some(kind) = OptKind::from_manifest(&decl.kind) else {
            warn_drop_option(skill, &decl.name, "unknown type");
            continue;
        };
        // Option names share the command-name charset; slug + de-dup so a name
        // with spaces/punctuation/case can never 400 the whole command.
        let name = discord_command_slug(&decl.name);
        if name.is_empty() || !seen.insert(name.clone()) {
            warn_drop_option(skill, &decl.name, "empty or duplicate option name");
            continue;
        }
        // Drop choices whose value doesn't fit the option type (a string value
        // on an int/number option is rejected by Discord).
        let choices = decl
            .choices
            .iter()
            .filter(|c| match kind {
                OptKind::Integer => c.value.parse::<i64>().is_ok(),
                OptKind::Number => c.value.parse::<f64>().is_ok(),
                _ => true,
            })
            .map(|c| Choice {
                name: c.name.clone(),
                value: c.value.clone(),
            })
            .collect();
        // Drop inverted numeric bounds rather than letting Discord 400 the command.
        let (mut min, mut max) = (decl.min, decl.max);
        if let (Some(lo), Some(hi)) = (min, max)
            && lo > hi
        {
            warn_drop_option(skill, &decl.name, "min greater than max; dropping bounds");
            min = None;
            max = None;
        }
        options.push(OptionSpec {
            name,
            description: decl.description.chars().take(100).collect(),
            description_localizations: valid_discord_localizations(
                &decl.description_localizations,
                &skill.name,
                &decl.name,
            ),
            kind,
            required: decl.required,
            choices,
            min,
            max,
            min_length: decl.min_length,
            max_length: decl.max_length,
        });
    }
    // Discord requires required options to precede optional ones; stable sort
    // keeps declaration order within each group.
    options.sort_by_key(|o| !o.required);
    options
}

fn warn_drop_option(skill: &zeroclaw_runtime::skills::Skill, option: &str, reason: &str) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
            .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
            .with_attrs(::serde_json::json!({
                "skill": skill.name,
                "option": option,
                "reason": reason,
            })),
        "dropping invalid skill slash option"
    );
}

/// Discord's supported command-localization locale codes
/// (<https://discord.com/developers/docs/reference#locales>). Registering with
/// any other key is a 400 that would wedge the reconcile in a retry loop, so a
/// skill-authored localization map is filtered to these before registration.
const DISCORD_LOCALES: &[&str] = &[
    "id", "da", "de", "en-GB", "en-US", "es-ES", "es-419", "fr", "hr", "it", "lt", "hu", "nl",
    "no", "pl", "pt-BR", "ro", "fi", "sv-SE", "vi", "tr", "cs", "el", "bg", "ru", "uk", "hi", "th",
    "zh-CN", "ja", "zh-TW", "ko",
];

/// Filter a skill-authored localization map to Discord-supported locale codes
/// (case-sensitive, as Discord requires) and truncate each value to Discord's
/// 100-char description limit. An authoring typo drops just that entry with a
/// WARN rather than 400-ing the whole command registration; an empty result
/// lets the caller omit the `*_localizations` key entirely.
fn valid_discord_localizations(
    raw: &std::collections::BTreeMap<String, String>,
    skill: &str,
    context: &str,
) -> std::collections::BTreeMap<String, String> {
    raw.iter()
        .filter(|(loc, _)| {
            if DISCORD_LOCALES.contains(&loc.as_str()) {
                true
            } else {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "skill": skill,
                            "context": context,
                            "locale": loc,
                        })),
                    "dropping unsupported Discord locale from slash localization"
                );
                false
            }
        })
        .map(|(k, v)| (k.clone(), v.chars().take(100).collect()))
        .collect()
}

/// Build the agent prompt for a skill slash command: the legacy single `input`
/// for an untyped command, or the submitted `name: value` lines for a typed
/// one. Returns `None` only when an *untyped* command was invoked with empty
/// input — a typed command, even an all-optional one invoked with no arguments,
/// still invokes the skill (rendering an empty argument list).
pub(crate) fn skill_command_prompt(
    spec: &DiscordSlashCommandSpec,
    input: &str,
    submitted: &[(String, String)],
) -> Option<String> {
    if spec.options.is_empty() {
        if input.is_empty() {
            return None;
        }
        return Some(format!(
            "Use the '{}' skill for this request: {input}",
            spec.skill_name
        ));
    }
    let rendered = submitted
        .iter()
        .map(|(name, value)| format!("{name}: {value}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(if rendered.is_empty() {
        format!("Use the '{}' skill for this request.", spec.skill_name)
    } else {
        format!(
            "Use the '{}' skill for this request:\n{rendered}",
            spec.skill_name
        )
    })
}

/// Apply the per-application command cap, logging the dropped slugs.
fn map_options_cap(specs: &mut Vec<DiscordSlashCommandSpec>) {
    if specs.len() > MAX_SKILL_SLASH_COMMANDS {
        let dropped: Vec<&str> = specs[MAX_SKILL_SLASH_COMMANDS..]
            .iter()
            .map(|s| s.slug.as_str())
            .collect();
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"dropped": dropped})),
            "too many skill slash commands; truncating to the registration cap"
        );
        specs.truncate(MAX_SKILL_SLASH_COMMANDS);
    }
}

/// The desired global-command set: `/ask` plus one command per skill spec,
/// each taking a single required string `input`. Also the registration
/// fingerprint input — its JSON string hashes into the skip-if-unchanged
/// gate.
pub(crate) fn slash_command_registration_body(
    specs: &[DiscordSlashCommandSpec],
) -> serde_json::Value {
    let mut ask = json!({
        "name": "ask",
        "description": "Ask the agent a question",
        "type": 1, // CHAT_INPUT
        "options": [{
            "name": "prompt",
            "description": "What to ask",
            "type": 3, // STRING
            "required": true
        }]
    });
    if let Some(loc) = localizations_object(builtin_localizations::ASK_COMMAND) {
        ask["description_localizations"] = loc;
    }
    if let Some(loc) = localizations_object(builtin_localizations::ASK_PROMPT_OPTION) {
        ask["options"][0]["description_localizations"] = loc;
    }
    let mut commands = vec![ask];
    for spec in specs {
        // A skill that declares no typed options keeps the legacy single
        // required string `input` (backward-compatible + the ownership marker
        // for reaping); one that declares options registers them instead.
        let options: Vec<serde_json::Value> = if spec.options.is_empty() {
            let mut input = json!({
                "name": "input",
                "description": SKILL_COMMAND_OPTION_DESCRIPTION,
                "type": 3, // STRING
                "required": true
            });
            if let Some(loc) = localizations_object(builtin_localizations::SKILL_INPUT_OPTION) {
                input["description_localizations"] = loc;
            }
            vec![input]
        } else {
            spec.options
                .iter()
                .map(OptionSpec::to_registration_json)
                .collect()
        };
        let mut cmd = json!({
            "name": spec.slug,
            "description": spec.description,
            "type": 1, // CHAT_INPUT
            "options": options,
        });
        if !spec.description_localizations.is_empty() {
            cmd["description_localizations"] = json!(spec.description_localizations);
        }
        commands.push(cmd);
    }
    serde_json::Value::Array(commands)
}

/// The option description this feature writes on every skill command. It
/// doubles as the ownership marker for stale-command reaping: Discord has
/// no durable "registered by" field, and a structural shape alone (one
/// required string option named `input`) is generic enough that foreign
/// tooling could collide with it.
pub(crate) const SKILL_COMMAND_OPTION_DESCRIPTION: &str = "What to send to the skill";

/// Compiled-in Discord-locale translations for the channel's own built-in
/// command/option descriptions. Compiled in (rather than sourced from the i18n
/// FTL machinery) because that machinery only compiles in `en`/`zh-CN` - es/fr/ja
/// load from a runtime locale dir that may not be deployed, so a stock binary
/// would silently drop them. `en` is the default (the literal description) and
/// is omitted. Initial translations - open to native-speaker refinement; skill
/// authors localize their own commands via the skill manifest.
mod builtin_localizations {
    /// `/ask` command description ("Ask the agent a question").
    pub(super) const ASK_COMMAND: &[(&str, &str)] = &[
        ("es-ES", "Hazle una pregunta al agente"),
        ("fr", "Poser une question à l'agent"),
        ("ja", "エージェントに質問する"),
        ("zh-CN", "向智能体提问"),
    ];
    /// `/ask` `prompt` option description ("What to ask").
    pub(super) const ASK_PROMPT_OPTION: &[(&str, &str)] = &[
        ("es-ES", "Qué preguntar"),
        ("fr", "Que demander"),
        ("ja", "質問内容"),
        ("zh-CN", "要问什么"),
    ];
    /// Default skill `input` option description (`SKILL_COMMAND_OPTION_DESCRIPTION`).
    pub(super) const SKILL_INPUT_OPTION: &[(&str, &str)] = &[
        ("es-ES", "Qué enviar a la habilidad"),
        ("fr", "Que envoyer à la compétence"),
        ("ja", "スキルに送る内容"),
        ("zh-CN", "发送给技能的内容"),
    ];
}

/// Build a Discord `*_localizations` object from a `(discord_locale, text)`
/// table. Empty input → `None`, so the caller omits the key entirely and the
/// command body stays byte-stable when there are no translations (preserving
/// the reconcile no-op for un-localized commands).
fn localizations_object(entries: &[(&str, &str)]) -> Option<serde_json::Value> {
    if entries.is_empty() {
        return None;
    }
    let map: serde_json::Map<String, serde_json::Value> = entries
        .iter()
        // Clamp to Discord's 100-char description limit, as the skill-authored
        // path does (`valid_discord_localizations`): a built-in translation that
        // ever exceeds it would otherwise 400 the registration and wedge the
        // reconcile in a retry loop.
        .map(|(loc, text)| {
            (
                (*loc).to_string(),
                json!(text.chars().take(100).collect::<String>()),
            )
        })
        .collect();
    Some(serde_json::Value::Object(map))
}

/// Ownership fingerprint for commands this feature owns: exactly one
/// required string option named `input` carrying this feature's exact
/// option description. Used to reap commands for uninstalled skills across
/// restarts; commands registered by other tooling must never be touched —
/// the description match makes accidental collision with a foreign
/// `/x <input>` command effectively impossible.
///
/// Limitation: two slash-enabled aliases sharing one bot token would see
/// each other's commands as reap candidates (commands are
/// application-global, desired sets are per-alias). Enable slash commands
/// on at most one alias per bot application.
///
/// Limitation (typed options): this recognizes only the legacy single-`input`
/// shape. A skill command that declares typed options has a different shape and
/// is therefore NOT auto-reaped when its skill is uninstalled — it is still
/// upserted/updated normally while installed. This is deliberately conservative
/// (it never risks deleting a foreign command); durable reaping of typed
/// commands (persisting the registered slug set) is a follow-on.
pub(crate) fn is_skill_command_shape(cmd: &serde_json::Value) -> bool {
    let Some(opts) = cmd.get("options").and_then(|o| o.as_array()) else {
        return false;
    };
    if opts.len() != 1 {
        return false;
    }
    let o = &opts[0];
    o.get("name").and_then(|n| n.as_str()) == Some("input")
        && o.get("type").and_then(serde_json::Value::as_u64) == Some(3)
        && o.get("required").and_then(serde_json::Value::as_bool) == Some(true)
        && o.get("description").and_then(|d| d.as_str()) == Some(SKILL_COMMAND_OPTION_DESCRIPTION)
}

/// Comparable projection of a command for change detection: description plus
/// per-option (name, type, required, description) and, for typed options, the
/// (choices, min/max value, min/max length, autocomplete) constraints. Discord
/// decorates
/// listed commands with server-side fields (id, version,
/// default_member_permissions, …) that must not defeat the comparison; the
/// numeric constraints are normalised (numbers → f64, lengths → u64, choice
/// values → number-or-string) so an int-vs-float representation difference
/// between what we send and what Discord echoes back doesn't force a spurious
/// re-registration.
pub(crate) fn command_projection(cmd: &serde_json::Value) -> serde_json::Value {
    json!({
        "description": cmd.get("description").cloned().unwrap_or_default(),
        // Localizations participate in change detection (with the GET's
        // `with_localizations=true`): Discord echoes `null` when none, which
        // equals our omitted key - so un-localized commands stay a no-op while
        // a translation change forces exactly one re-registration.
        "description_localizations": cmd
            .get("description_localizations")
            .cloned()
            .unwrap_or(serde_json::Value::Null),
        "options": cmd
            .get("options")
            .and_then(|o| o.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|o| {
                        json!({
                            "name": o.get("name").cloned().unwrap_or_default(),
                            "type": o.get("type").cloned().unwrap_or_default(),
                            "required": o.get("required").cloned().unwrap_or(json!(false)),
                            "description": o.get("description").cloned().unwrap_or_default(),
                            "description_localizations": o
                                .get("description_localizations")
                                .cloned()
                                .unwrap_or(serde_json::Value::Null),
                            "choices": o
                                .get("choices")
                                .and_then(|c| c.as_array())
                                .map(|cs| {
                                    cs.iter()
                                        .map(|c| {
                                            json!({
                                                "name": c.get("name").cloned().unwrap_or_default(),
                                                "value": normalize_scalar(c.get("value")),
                                            })
                                        })
                                        .collect::<Vec<_>>()
                                })
                                .unwrap_or_default(),
                            "min_value": o.get("min_value").and_then(serde_json::Value::as_f64),
                            "max_value": o.get("max_value").and_then(serde_json::Value::as_f64),
                            "min_length": o.get("min_length").and_then(serde_json::Value::as_u64),
                            "max_length": o.get("max_length").and_then(serde_json::Value::as_u64),
                            // Autocomplete options omit static `choices`; project
                            // the flag (default false) so an autocomplete option
                            // we registered compares equal to Discord's echo and
                            // doesn't trigger a spurious re-registration.
                            "autocomplete": o
                                .get("autocomplete")
                                .and_then(serde_json::Value::as_bool)
                                .unwrap_or(false),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
}

/// Normalise a choice value for projection: a JSON number becomes an `f64`
/// (so `10` and `10.0` compare equal), anything else is kept as-is.
fn normalize_scalar(v: Option<&serde_json::Value>) -> serde_json::Value {
    match v {
        Some(serde_json::Value::Number(n)) => json!(n.as_f64()),
        Some(other) => other.clone(),
        None => serde_json::Value::Null,
    }
}

/// Discord REST base; injectable in `reconcile_slash_commands` for tests.
pub(crate) const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Turn a `429` response into a unix-seconds deadline before which no further
/// reconcile should run, reading Discord's `retry_after` body / headers.
async fn rate_limit_deadline(resp: reqwest::Response) -> i64 {
    let now = crate::discord_slash_state::now_unix();
    let headers = resp.headers().clone();
    let body = resp.json::<serde_json::Value>().await.ok();
    crate::discord_slash_state::retry_after_deadline(&headers, body.as_ref(), now)
}

/// Reconcile the application's global commands with the desired set:
/// upsert each desired command (POST upserts by name) and delete stale
/// skill-shaped commands left over from uninstalled skills. Commands
/// registered by other tooling are never touched — this deliberately
/// avoids the bulk-overwrite PUT. Global commands can take up to an hour
/// to propagate the first time.
///
/// Returns `Err` when any owned stale command could not be deleted (other
/// than a 404, which means it is already gone): the caller's fingerprint
/// must not record such a pass as successful, or the stale command would
/// never be retried while the desired set stays unchanged. Upserts for the
/// desired set are still attempted first so a delete failure cannot block
/// new registrations.
pub(crate) async fn reconcile_slash_commands(
    client: &reqwest::Client,
    bot_token: &str,
    app_id: &str,
    desired: &serde_json::Value,
    api_base: &str,
    scope: SlashScope,
    guild_ids: &[String],
) -> anyhow::Result<ReconcileOutcome> {
    let auth = format!("Bot {bot_token}");
    let Some(desired) = desired.as_array() else {
        anyhow::bail!("desired command set is not an array");
    };
    let desired_names: std::collections::HashSet<&str> = desired
        .iter()
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
        .collect();

    let global_base = format!("{api_base}/applications/{app_id}/commands");
    let guild_base = |g: &str| format!("{api_base}/applications/{app_id}/guilds/{g}/commands");
    // Active = where the commands live now; inactive = the other scope, whose
    // leftover skill commands we reap so flipping `slash_command_scope` never
    // leaves the same command registered in both places (the guild-scope
    // migration hazard). `guild_ids` drives the active set under Guild and the
    // reap set under Global.
    let (active, inactive): (Vec<String>, Vec<String>) = match scope {
        SlashScope::Global => (
            vec![global_base],
            guild_ids.iter().map(|g| guild_base(g)).collect(),
        ),
        SlashScope::Guild => (
            guild_ids.iter().map(|g| guild_base(g)).collect(),
            vec![global_base],
        ),
    };
    // The canonical `/ask` we would register, used to prove ownership before
    // reaping a `/ask` from the inactive scope (#7922): a foreign `/ask` whose
    // projection differs is left untouched.
    let expected_ask = desired
        .iter()
        .find(|c| c.get("name").and_then(|n| n.as_str()) == Some("ask"));
    // Best-effort cleanup of the now-inactive scope first; a 429 surfaces the
    // cooldown like any active-scope pass would.
    for base in &inactive {
        if let ReconcileOutcome::RateLimited { until } =
            reap_all_owned_commands(client, &auth, base, expected_ask).await?
        {
            return Ok(ReconcileOutcome::RateLimited { until });
        }
    }
    // Reconcile each active endpoint (one for Global; one per guild for Guild).
    for base in &active {
        if let ReconcileOutcome::RateLimited { until } =
            reconcile_one_endpoint(client, &auth, base, desired, &desired_names).await?
        {
            return Ok(ReconcileOutcome::RateLimited { until });
        }
    }
    Ok(ReconcileOutcome::Reconciled)
}

/// Reap every command this channel owns (`/ask` + skill-shaped) from an
/// endpoint without upserting - used to clear the inactive scope after a
/// `slash_command_scope` switch. Best-effort: a failed listing (e.g. the bot
/// lacks `applications.commands` in a guild it has left) is logged and skipped,
/// never fatal to the active-scope reconcile.
async fn reap_all_owned_commands(
    client: &reqwest::Client,
    auth: &str,
    base: &str,
    expected_ask: Option<&serde_json::Value>,
) -> anyhow::Result<ReconcileOutcome> {
    // `with_localizations=true` so the listing echoes the full `*_localizations`
    // dictionaries; without it Discord returns them null and our `/ask`
    // ownership check below (a projection match against the command we register,
    // which carries localizations) would never match our own `/ask`.
    let resp = client
        .get(format!("{base}?with_localizations=true"))
        .header("Authorization", auth)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(ReconcileOutcome::RateLimited {
            until: rate_limit_deadline(resp).await,
        });
    }
    if !resp.status().is_success() {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"status": resp.status().as_u16()})),
            "inactive-scope command listing failed; skipping cross-scope cleanup"
        );
        return Ok(ReconcileOutcome::Reconciled);
    }
    // Best-effort: a malformed listing body must not abort the (more important)
    // active-scope reconcile - log and skip cross-scope cleanup, as for a failed
    // listing status above.
    let existing: Vec<serde_json::Value> = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"err": e.without_url().to_string()})),
                "inactive-scope command listing returned an unparseable body; skipping cross-scope cleanup"
            );
            return Ok(ReconcileOutcome::Reconciled);
        }
    };
    for cmd in &existing {
        let name = cmd.get("name").and_then(|n| n.as_str()).unwrap_or("");
        // Only reap a `/ask` that is *ours* - one whose projection matches the
        // `/ask` we register (#7922). Deleting by name alone would reap a `/ask`
        // registered by other tooling that happens to share the inactive scope.
        // Skill commands keep their own shape-based ownership marker.
        let is_owned_ask = name == "ask"
            && expected_ask.is_some_and(|a| command_projection(cmd) == command_projection(a));
        if !is_owned_ask && !is_skill_command_shape(cmd) {
            continue;
        }
        let Some(id) = cmd.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let del = client
            .delete(format!("{base}/{id}"))
            .header("Authorization", auth)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if del.status().is_success() || del.status() == reqwest::StatusCode::NOT_FOUND {
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"command": name})),
                "reaped command from inactive slash scope"
            );
        } else {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "command": name,
                        "status": del.status().as_u16(),
                    })),
                "failed to reap command from inactive slash scope (best-effort)"
            );
        }
    }
    Ok(ReconcileOutcome::Reconciled)
}

/// Reconcile the skill command set at a single endpoint (`base`): reap stale
/// skill commands, then upsert each desired command whose projection differs
/// from what's registered. Steady-state restarts converge to ~zero writes.
async fn reconcile_one_endpoint(
    client: &reqwest::Client,
    auth: &str,
    base: &str,
    desired: &[serde_json::Value],
    desired_names: &std::collections::HashSet<&str>,
) -> anyhow::Result<ReconcileOutcome> {
    // Reap stale skill commands first so the 100-command cap never blocks
    // the upserts that follow. Delete failures are counted, not fatal
    // mid-pass: the upserts still run, but the pass reports Err at the end
    // so the fingerprint is not recorded and the next READY retries.
    let mut failed_deletes = 0usize;
    // `with_localizations=true` so the listing echoes back the full
    // `*_localizations` dictionaries; without it Discord returns them null and
    // every localized command would mismatch the projection and re-register on
    // each READY (burning the daily command-create budget).
    let resp = client
        .get(format!("{base}?with_localizations=true"))
        .header("Authorization", auth)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(ReconcileOutcome::RateLimited {
            until: rate_limit_deadline(resp).await,
        });
    }
    if !resp.status().is_success() {
        anyhow::bail!("listing commands failed ({})", resp.status());
    }
    let existing: Vec<serde_json::Value> =
        resp.json().await.map_err(reqwest::Error::without_url)?;
    for cmd in &existing {
        let name = cmd.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name == "ask" || desired_names.contains(name) || !is_skill_command_shape(cmd) {
            continue;
        }
        let Some(id) = cmd.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        let del = client
            .delete(format!("{base}/{id}"))
            .header("Authorization", auth)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if del.status().is_success() || del.status() == reqwest::StatusCode::NOT_FOUND {
            // 404 = already gone (raced another reconcile or manual
            // cleanup) — the desired end state holds either way.
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({"command": name})),
                "deregistered stale skill slash command"
            );
        } else {
            failed_deletes += 1;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "command": name,
                        "status": del.status().as_u16(),
                    })),
                "failed to deregister stale skill slash command"
            );
        }
    }

    let existing_by_name: std::collections::HashMap<&str, &serde_json::Value> = existing
        .iter()
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()).map(|n| (n, c)))
        .collect();
    let mut upserted = 0usize;
    for cmd in desired {
        let name = cmd.get("name").and_then(|n| n.as_str()).unwrap_or("?");
        // Steady-state restarts should be ~zero writes: Discord's daily
        // command-create budget is finite, and the existing list is already
        // in hand.
        if let Some(current) = existing_by_name.get(name)
            && command_projection(current) == command_projection(cmd)
        {
            continue;
        }
        let resp = client
            .post(base)
            .header("Authorization", auth)
            .json(cmd)
            .send()
            .await
            .map_err(reqwest::Error::without_url)?;
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            // Stop on the first 429 and surface the cooldown rather than
            // hammering the remaining upserts into the same rate limit.
            let until = rate_limit_deadline(resp).await;
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"command": name, "retry_after_until": until})),
                "discord slash command reconcile rate-limited; backing off"
            );
            return Ok(ReconcileOutcome::RateLimited { until });
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("slash command registration failed for '{name}' ({status}): {err}");
        }
        upserted += 1;
    }
    if upserted > 0 {
        ::zeroclaw_log::record!(
            INFO,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(::serde_json::json!({"upserted": upserted})),
            "discord slash commands upserted"
        );
    }
    if failed_deletes > 0 {
        anyhow::bail!(
            "{failed_deletes} stale skill command delete(s) failed; \
             reconcile not recorded, next READY retries"
        );
    }
    Ok(ReconcileOutcome::Reconciled)
}

#[cfg(test)]
mod typed_option_tests {
    use super::super::slash_options::{OptKind, OptionSpec};
    use super::*;

    fn spec_with(options: Vec<OptionSpec>) -> DiscordSlashCommandSpec {
        DiscordSlashCommandSpec {
            skill_name: "s".to_string(),
            slug: "s".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            options,
        }
    }

    fn opt(name: &str, kind: OptKind, required: bool) -> OptionSpec {
        OptionSpec {
            name: name.to_string(),
            description: name.to_string(),
            description_localizations: Default::default(),
            kind,
            required,
            choices: Vec::new(),
            min: None,
            max: None,
            min_length: None,
            max_length: None,
        }
    }

    #[test]
    fn no_options_falls_back_to_the_legacy_input() {
        let body = slash_command_registration_body(&[spec_with(Vec::new())]);
        let cmd = &body.as_array().unwrap()[1]; // [0] is /ask
        let opts = cmd["options"].as_array().unwrap();
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0]["name"], json!("input"));
        assert_eq!(opts[0]["type"], json!(3));
        assert_eq!(
            opts[0]["description"],
            json!(SKILL_COMMAND_OPTION_DESCRIPTION)
        );
    }

    #[test]
    fn builtin_commands_carry_compiled_in_localizations() {
        let body = slash_command_registration_body(&[spec_with(Vec::new())]);
        let cmds = body.as_array().unwrap();
        // /ask command + its prompt option are localized.
        let ask = &cmds[0];
        assert_eq!(ask["name"], json!("ask"));
        assert_eq!(
            ask["description_localizations"]["fr"],
            json!("Poser une question à l'agent")
        );
        assert_eq!(
            ask["options"][0]["description_localizations"]["ja"],
            json!("質問内容")
        );
        // The default skill `input` option is localized too, while keeping its
        // canonical (English) description as the reap-ownership marker.
        let input = &cmds[1]["options"][0];
        assert_eq!(input["name"], json!("input"));
        assert_eq!(
            input["description"],
            json!(SKILL_COMMAND_OPTION_DESCRIPTION)
        );
        assert!(input["description_localizations"]["zh-CN"].is_string());
    }

    #[test]
    fn skill_localizations_flow_through_and_bad_locales_are_dropped() {
        use std::collections::BTreeMap;
        let mut cmd_loc = BTreeMap::new();
        cmd_loc.insert("fr".to_string(), "Vérifier le déploiement".to_string());
        // An authoring typo must be dropped, not 400 the whole registration.
        cmd_loc.insert("xx-INVALID".to_string(), "ignored".to_string());
        let mut opt_loc = BTreeMap::new();
        opt_loc.insert("ja".to_string(), "クエリ".to_string());

        let mut option = sso("query", "string");
        option.description_localizations = opt_loc;
        let mut skill = skill_with(vec![option]);
        skill.description_localizations = cmd_loc;

        let specs = discord_slash_specs_from_skills(std::slice::from_ref(&skill));
        let spec = &specs[0];
        assert_eq!(
            spec.description_localizations.get("fr").map(String::as_str),
            Some("Vérifier le déploiement")
        );
        assert!(!spec.description_localizations.contains_key("xx-INVALID"));

        let body = slash_command_registration_body(&specs);
        let cmd = &body.as_array().unwrap()[1]; // [0] is /ask
        assert_eq!(
            cmd["description_localizations"]["fr"],
            json!("Vérifier le déploiement")
        );
        assert!(cmd["description_localizations"].get("xx-INVALID").is_none());
        assert_eq!(
            cmd["options"][0]["description_localizations"]["ja"],
            json!("クエリ")
        );
    }

    #[test]
    fn typed_options_replace_the_legacy_input() {
        let mut limit = opt("limit", OptKind::Integer, false);
        limit.min = Some(1.0);
        limit.max = Some(50.0);
        let mut query = opt("query", OptKind::String, true);
        query.min_length = Some(1);
        let body = slash_command_registration_body(&[spec_with(vec![query, limit])]);
        let opts = body.as_array().unwrap()[1]["options"].as_array().unwrap();
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0]["name"], json!("query"));
        assert_eq!(opts[0]["type"], json!(3));
        assert_eq!(opts[0]["min_length"], json!(1));
        assert_eq!(opts[1]["name"], json!("limit"));
        assert_eq!(opts[1]["type"], json!(4));
        assert_eq!(opts[1]["min_value"], json!(1));
        assert_eq!(opts[1]["max_value"], json!(50));
    }

    #[test]
    fn md_skill_frontmatter_options_drive_a_typed_command_end_to_end() {
        // End-to-end proof that no channel-side change was needed: a SKILL.md
        // declaring slash_options in its frontmatter loads through the public
        // runtime loader and registers a typed Discord command (dropdown +
        // integer range), not the legacy single `input`.
        let tmp = tempfile::tempdir().unwrap();
        let skill_dir = tmp.path().join("draft");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let md = r#"---
name: draft
description: Draft content to a spec.
tags: [slash]
slash_options:
  - name: format
    description: Output format.
    type: string
    required: true
    choices: [{name: Email, value: email}, {name: Tweet, value: tweet}]
  - name: words
    type: integer
    min: 10
    max: 2000
---
# Draft

Write it.
"#;
        std::fs::write(skill_dir.join("SKILL.md"), md).unwrap();

        let (skills, _) = zeroclaw_runtime::skills::load_skills_from_directory(tmp.path(), false);
        let specs = discord_slash_specs_from_skills(&skills);
        assert_eq!(
            specs.len(),
            1,
            "the slash-tagged MD skill yields one command"
        );

        let body = slash_command_registration_body(&specs);
        let arr = body.as_array().unwrap();
        let draft = arr
            .iter()
            .find(|c| c["name"] == json!("draft"))
            .expect("draft command present");
        let opts = draft["options"].as_array().unwrap();

        // Typed options replaced the legacy single `input`.
        assert!(opts.iter().all(|o| o["name"] != json!("input")));
        let format = opts
            .iter()
            .find(|o| o["name"] == json!("format"))
            .expect("format option");
        assert_eq!(format["type"], json!(3)); // STRING
        assert_eq!(format["required"], json!(true));
        assert_eq!(format["choices"].as_array().unwrap().len(), 2);
        let words = opts
            .iter()
            .find(|o| o["name"] == json!("words"))
            .expect("words option");
        assert_eq!(words["type"], json!(4)); // INTEGER
        assert_eq!(words["min_value"], json!(10));
        assert_eq!(words["max_value"], json!(2000));
    }

    #[test]
    fn projection_is_stable_across_int_vs_float_bounds() {
        // What we send (integer min/max) vs what Discord might echo back (float),
        // plus Discord's server-side decorations — must project equal.
        let sent = json!({ "description": "d", "options": [
            { "name": "limit", "type": 4, "required": false, "description": "l", "min_value": 1, "max_value": 50 }
        ]});
        let echoed = json!({ "description": "d", "id": "9", "version": "v", "options": [
            { "name": "limit", "type": 4, "required": false, "description": "l", "min_value": 1.0, "max_value": 50.0 }
        ]});
        assert_eq!(command_projection(&sent), command_projection(&echoed));
    }

    #[test]
    fn map_drops_unknown_kinds_and_sorts_required_first() {
        let skill = zeroclaw_runtime::skills::Skill {
            name: "s".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "0".to_string(),
            author: None,
            tags: vec!["slash".to_string()],
            tools: Vec::new(),
            prompts: Vec::new(),
            slash_options: vec![
                zeroclaw_runtime::skills::SkillSlashOption {
                    name: "opt".to_string(),
                    description: "o".to_string(),
                    kind: "string".to_string(),
                    required: false,
                    description_localizations: Default::default(),
                    choices: Vec::new(),
                    min: None,
                    max: None,
                    min_length: None,
                    max_length: None,
                },
                zeroclaw_runtime::skills::SkillSlashOption {
                    name: "Req".to_string(),
                    description: "r".to_string(),
                    kind: "integer".to_string(),
                    required: true,
                    description_localizations: Default::default(),
                    choices: Vec::new(),
                    min: None,
                    max: None,
                    min_length: None,
                    max_length: None,
                },
                zeroclaw_runtime::skills::SkillSlashOption {
                    name: "bad".to_string(),
                    description: "b".to_string(),
                    kind: "bogus".to_string(),
                    required: false,
                    description_localizations: Default::default(),
                    choices: Vec::new(),
                    min: None,
                    max: None,
                    min_length: None,
                    max_length: None,
                },
            ],
            location: None,
        };
        let mapped = map_skill_slash_options(&skill);
        assert_eq!(mapped.len(), 2, "the unknown-kind option is dropped");
        assert_eq!(mapped[0].name, "req", "required first + name lower-cased");
        assert!(mapped[0].required);
        assert_eq!(mapped[1].name, "opt");
    }

    fn sso(name: &str, kind: &str) -> zeroclaw_runtime::skills::SkillSlashOption {
        zeroclaw_runtime::skills::SkillSlashOption {
            name: name.to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            kind: kind.to_string(),
            required: false,
            choices: Vec::new(),
            min: None,
            max: None,
            min_length: None,
            max_length: None,
        }
    }

    fn skill_with(
        slash_options: Vec<zeroclaw_runtime::skills::SkillSlashOption>,
    ) -> zeroclaw_runtime::skills::Skill {
        zeroclaw_runtime::skills::Skill {
            name: "s".to_string(),
            description: "d".to_string(),
            description_localizations: Default::default(),
            version: "0".to_string(),
            author: None,
            tags: vec!["slash".to_string()],
            tools: Vec::new(),
            prompts: Vec::new(),
            slash_options,
            location: None,
        }
    }

    #[test]
    fn map_slugs_option_names_and_dedups() {
        let mapped = map_skill_slash_options(&skill_with(vec![
            sso("My Option", "string"), // -> "my-option"
            sso("dup", "string"),
            sso("DUP", "string"), // slugs to "dup" -> dropped as duplicate
        ]));
        assert_eq!(mapped.len(), 2);
        assert_eq!(mapped[0].name, "my-option");
        assert_eq!(mapped[1].name, "dup");
    }

    #[test]
    fn map_drops_a_nonparsing_numeric_choice() {
        let mut o = sso("n", "integer");
        o.choices = vec![
            zeroclaw_runtime::skills::SkillSlashChoice {
                name: "ok".to_string(),
                value: "10".to_string(),
            },
            zeroclaw_runtime::skills::SkillSlashChoice {
                name: "bad".to_string(),
                value: "3.14".to_string(),
            },
        ];
        let mapped = map_skill_slash_options(&skill_with(vec![o]));
        assert_eq!(
            mapped[0].choices.len(),
            1,
            "the non-integer choice is dropped"
        );
        assert_eq!(mapped[0].choices[0].value, "10");
    }

    #[test]
    fn map_drops_inverted_bounds() {
        let mut o = sso("n", "integer");
        o.min = Some(50.0);
        o.max = Some(1.0);
        let mapped = map_skill_slash_options(&skill_with(vec![o]));
        assert!(mapped[0].min.is_none() && mapped[0].max.is_none());
    }

    #[test]
    fn prompt_legacy_typed_and_all_optional() {
        let legacy = spec_with(Vec::new());
        assert_eq!(skill_command_prompt(&legacy, "", &[]), None);
        assert_eq!(
            skill_command_prompt(&legacy, "do it", &[]).unwrap(),
            "Use the 's' skill for this request: do it"
        );

        let typed = spec_with(vec![opt("q", OptKind::String, true)]);
        // all-optional / no args STILL invokes the skill (the bug fix)
        assert_eq!(
            skill_command_prompt(&typed, "", &[]).unwrap(),
            "Use the 's' skill for this request."
        );
        let submitted = vec![("q".to_string(), "rust".to_string())];
        assert_eq!(
            skill_command_prompt(&typed, "", &submitted).unwrap(),
            "Use the 's' skill for this request:\nq: rust"
        );
    }
}
