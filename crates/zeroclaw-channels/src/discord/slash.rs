//! Slash-command registration + reconcile (the prototype `/ask` plus one
//! command per `slash`-tagged skill). Discord delivers application-command
//! interactions over the same Gateway WebSocket as INTERACTION_CREATE; this
//! module owns deriving the desired command set from installed skills and
//! reconciling it against Discord's REST API — idempotent upsert + stale-command
//! reaping, with persisted-fingerprint and `Retry-After` durability via
//! `discord_slash_state`. The READY-time orchestration, the dispatch arm, and
//! the interaction callbacks live in `mod.rs` / `interaction`.

use serde_json::json;

use super::types::{DiscordSlashCommandSpec, ReconcileOutcome};

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
        });
    }
    specs.sort_by(|a, b| a.slug.cmp(&b.slug));
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
    specs
}

/// The desired global-command set: `/ask` plus one command per skill spec,
/// each taking a single required string `input`. Also the registration
/// fingerprint input — its JSON string hashes into the skip-if-unchanged
/// gate.
pub(crate) fn slash_command_registration_body(
    specs: &[DiscordSlashCommandSpec],
) -> serde_json::Value {
    let mut commands = vec![json!({
        "name": "ask",
        "description": "Ask the agent a question",
        "type": 1, // CHAT_INPUT
        "options": [{
            "name": "prompt",
            "description": "What to ask",
            "type": 3, // STRING
            "required": true
        }]
    })];
    for spec in specs {
        commands.push(json!({
            "name": spec.slug,
            "description": spec.description,
            "type": 1, // CHAT_INPUT
            "options": [{
                "name": "input",
                "description": SKILL_COMMAND_OPTION_DESCRIPTION,
                "type": 3, // STRING
                "required": true
            }]
        }));
    }
    serde_json::Value::Array(commands)
}

/// The option description this feature writes on every skill command. It
/// doubles as the ownership marker for stale-command reaping: Discord has
/// no durable "registered by" field, and a structural shape alone (one
/// required string option named `input`) is generic enough that foreign
/// tooling could collide with it.
pub(crate) const SKILL_COMMAND_OPTION_DESCRIPTION: &str = "What to send to the skill";

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
/// (name, type, required, description) per option. Discord decorates listed
/// commands with server-side fields (id, version, default_member_permissions,
/// …) that must not defeat the comparison.
pub(crate) fn command_projection(cmd: &serde_json::Value) -> serde_json::Value {
    json!({
        "description": cmd.get("description").cloned().unwrap_or_default(),
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
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
    })
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
) -> anyhow::Result<ReconcileOutcome> {
    let base = format!("{api_base}/applications/{app_id}/commands");
    let auth = format!("Bot {bot_token}");
    let Some(desired) = desired.as_array() else {
        anyhow::bail!("desired command set is not an array");
    };
    let desired_names: std::collections::HashSet<&str> = desired
        .iter()
        .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
        .collect();

    // Reap stale skill commands first so the 100-command cap never blocks
    // the upserts that follow. Delete failures are counted, not fatal
    // mid-pass: the upserts still run, but the pass reports Err at the end
    // so the fingerprint is not recorded and the next READY retries.
    let mut failed_deletes = 0usize;
    let resp = client
        .get(&base)
        .header("Authorization", &auth)
        .send()
        .await
        .map_err(reqwest::Error::without_url)?;
    if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Ok(ReconcileOutcome::RateLimited {
            until: rate_limit_deadline(resp).await,
        });
    }
    if !resp.status().is_success() {
        anyhow::bail!("listing global commands failed ({})", resp.status());
    }
    let existing: Vec<serde_json::Value> = resp.json().await?;
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
            .header("Authorization", &auth)
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
            .post(&base)
            .header("Authorization", &auth)
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
