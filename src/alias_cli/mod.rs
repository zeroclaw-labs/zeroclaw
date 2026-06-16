//! CLI for alias CRUD — `zeroclaw {agents,providers,channels} {create,list,
//! rename,delete}` (#7468 / #7175).
//!
//! Thin surface over the config-layer cascade in
//! [`zeroclaw_config::alias_refs`]: `rename_with_cascade` / `delete_with_cascade`
//! rewrite/scrub every reference and report the entry paths that changed; this
//! module marks each dirty and persists via `Config::save_dirty` (which writes
//! only marked paths). Plural groups (`agents`/`providers`/`channels`) are
//! distinct from the singular `agent <alias>` run command, which is untouched.
//!
//! Providers and channels carry no owned non-config state, so their delete/
//! rename is config-only. The agent owned-state cascade (memory / cron / acp /
//! session rows + the workspace dir) is wired in a follow-up; until then agent
//! delete/rename warn that owned state was not cascaded.

use anyhow::{Context, Result, bail};
use zeroclaw::{AgentsCommands, ChannelsCommands, ProvidersCommands};
use zeroclaw_config::alias_refs::{
    self, AliasKind, CascadeError, CascadePolicy, ProviderCategory, RenameError,
};
use zeroclaw_config::schema::Config;

/// The agent alias reserved as the runtime fallback — protected from delete
/// (rename is already guarded inside `rename_with_cascade`).
const RESERVED_DEFAULT_AGENT: &str = "default";

fn parse_provider_category(category: &str) -> Result<ProviderCategory> {
    match category {
        "models" => Ok(ProviderCategory::Models),
        "tts" => Ok(ProviderCategory::Tts),
        "transcription" => Ok(ProviderCategory::Transcription),
        other => {
            bail!("unknown provider category `{other}` (expected models | tts | transcription)")
        }
    }
}

/// The map-key section path for a kind (e.g. `agents`, `providers.models.anthropic`,
/// `channels.discord`).
fn section_path(kind: &AliasKind) -> String {
    match kind {
        AliasKind::Agent => "agents".to_string(),
        AliasKind::Provider { category, family } => {
            let cat = match category {
                ProviderCategory::Models => "models",
                ProviderCategory::Tts => "tts",
                ProviderCategory::Transcription => "transcription",
            };
            format!("providers.{cat}.{family}")
        }
        AliasKind::Channel { channel_type } => format!("channels.{channel_type}"),
    }
}

fn list_section(config: &Config, section: &str) -> Result<()> {
    match config.get_map_keys(section) {
        Some(mut keys) => {
            keys.sort();
            if keys.is_empty() {
                println!("(no entries under {section})");
            } else {
                for k in keys {
                    println!("{k}");
                }
            }
        }
        None => bail!("no such config section: {section}"),
    }
    Ok(())
}

fn create_entry(config: &mut Config, section: &str, alias: &str) -> Result<()> {
    if config
        .create_map_key(section, alias)
        .map_err(anyhow::Error::msg)?
    {
        config.mark_dirty(&format!("{section}.{alias}"));
        println!("created {section}.{alias}");
    } else {
        println!("{section}.{alias} already exists (no change)");
    }
    Ok(())
}

/// Print the dry-run impact (blockers + scrubs) for a delete.
fn print_impact(kind: &AliasKind, alias: &str, config: &Config) {
    let report = alias_refs::plan_delete(config, kind, alias);
    if report.blockers.is_empty() {
        println!(
            "deleting {}.{alias} would scrub {} reference(s):",
            section_path(kind),
            report.scrubs.len()
        );
    } else {
        println!(
            "deleting {}.{alias} is BLOCKED by {} hard reference(s):",
            section_path(kind),
            report.blockers.len()
        );
        for b in &report.blockers {
            println!("  ✗ {} (hard reference)", b.path);
        }
    }
    for s in &report.scrubs {
        println!("  • {} (would be scrubbed)", s.path);
    }
}

/// Delete an aliased entry's config references (config-layer only).
fn delete_config(
    config: &mut Config,
    kind: &AliasKind,
    alias: &str,
    dry_run: bool,
    yes: bool,
) -> Result<()> {
    let section = section_path(kind);
    if dry_run {
        print_impact(kind, alias, config);
        return Ok(());
    }
    if !yes {
        print_impact(kind, alias, config);
        println!("\nNo changes made. Re-run with --yes to apply (or --dry-run to preview).");
        return Ok(());
    }
    match alias_refs::delete_with_cascade(config, kind, alias, CascadePolicy::RefuseOnHard) {
        Ok(report) => {
            for path in report.dirty_paths() {
                config.mark_dirty(&path);
            }
            println!(
                "deleted {section}.{alias} (scrubbed {} reference(s))",
                report.applied.len()
            );
            Ok(())
        }
        Err(CascadeError::Refused(report)) => {
            println!(
                "refused: {} hard reference(s) block the delete:",
                report.blockers.len()
            );
            for b in &report.blockers {
                println!("  ✗ {}", b.path);
            }
            bail!("delete refused — resolve the hard references first");
        }
        Err(CascadeError::NotFound(p)) => bail!("{p} is not configured"),
        Err(e) => bail!("delete failed: {e}"),
    }
}

/// Rename an aliased entry's config references (config-layer only).
fn rename_config(config: &mut Config, kind: &AliasKind, from: &str, to: &str) -> Result<()> {
    match alias_refs::rename_with_cascade(config, kind, from, to) {
        Ok(report) => {
            for path in &report.dirty_paths {
                config.mark_dirty(path);
            }
            println!(
                "renamed {sec}.{from} → {sec}.{to} (rewrote {} reference path(s))",
                report.dirty_paths.len(),
                sec = section_path(kind)
            );
            Ok(())
        }
        Err(RenameError::NotFound(p)) => bail!("{p} is not configured"),
        Err(RenameError::InvalidName(m)) => bail!("invalid new alias: {m}"),
        Err(RenameError::Reserved(a)) => bail!("alias `{a}` is reserved and cannot be renamed"),
        Err(RenameError::PostCondition(m)) => bail!("rename cascade post-condition failed: {m}"),
    }
}

async fn save(config: &mut Config) -> Result<()> {
    Box::pin(config.save_dirty())
        .await
        .context("failed to persist config")
}

// ── agents ──────────────────────────────────────────────────────────────────

pub async fn handle_agents(cmd: AgentsCommands, config: &mut Config) -> Result<()> {
    match cmd {
        AgentsCommands::List => list_section(config, "agents"),
        AgentsCommands::Create { alias } => {
            create_entry(config, "agents", &alias)?;
            save(config).await
        }
        AgentsCommands::Rename { from, to } => {
            rename_config(config, &AliasKind::Agent, &from, &to)?;
            warn_agent_owned_state();
            save(config).await
        }
        AgentsCommands::Delete {
            alias,
            dry_run,
            yes,
        } => {
            if alias == RESERVED_DEFAULT_AGENT {
                bail!("the `default` agent is reserved and cannot be deleted");
            }
            delete_config(config, &AliasKind::Agent, &alias, dry_run, yes)?;
            if yes && !dry_run {
                warn_agent_owned_state();
                save(config).await?;
            }
            Ok(())
        }
    }
}

fn warn_agent_owned_state() {
    eprintln!(
        "note: config references were updated, but the agent's owned state \
         (memory rows, workspace dir, cron/acp/session rows) was NOT cascaded \
         by this CLI yet — use the gateway API for the full owned-state cascade."
    );
}

// ── providers ─────────────────────────────────────────────────────────────────

pub async fn handle_providers(cmd: ProvidersCommands, config: &mut Config) -> Result<()> {
    match cmd {
        ProvidersCommands::List { category } => {
            let cats = match category {
                Some(c) => vec![parse_provider_category(&c)?],
                None => vec![
                    ProviderCategory::Models,
                    ProviderCategory::Tts,
                    ProviderCategory::Transcription,
                ],
            };
            for cat in cats {
                let cat_name = match cat {
                    ProviderCategory::Models => "models",
                    ProviderCategory::Tts => "tts",
                    ProviderCategory::Transcription => "transcription",
                };
                // Enumerate families under this category, then their aliases.
                if let Some(families) = config.get_map_keys(&format!("providers.{cat_name}")) {
                    let mut families = families;
                    families.sort();
                    for family in families {
                        if let Some(mut aliases) =
                            config.get_map_keys(&format!("providers.{cat_name}.{family}"))
                        {
                            aliases.sort();
                            for a in aliases {
                                println!("{cat_name}.{family}.{a}");
                            }
                        }
                    }
                }
            }
            Ok(())
        }
        ProvidersCommands::Create {
            category,
            family,
            alias,
        } => {
            let cat = parse_provider_category(&category)?;
            let section = section_path(&AliasKind::Provider {
                category: cat,
                family,
            });
            create_entry(config, &section, &alias)?;
            save(config).await
        }
        ProvidersCommands::Rename {
            category,
            family,
            from,
            to,
        } => {
            let category = parse_provider_category(&category)?;
            rename_config(
                config,
                &AliasKind::Provider { category, family },
                &from,
                &to,
            )?;
            save(config).await
        }
        ProvidersCommands::Delete {
            category,
            family,
            alias,
            dry_run,
            yes,
        } => {
            let category = parse_provider_category(&category)?;
            let kind = AliasKind::Provider { category, family };
            delete_config(config, &kind, &alias, dry_run, yes)?;
            if yes && !dry_run {
                save(config).await?;
            }
            Ok(())
        }
    }
}

// ── channels ─────────────────────────────────────────────────────────────────

pub async fn handle_channels(cmd: ChannelsCommands, config: &mut Config) -> Result<()> {
    match cmd {
        ChannelsCommands::List { channel_type } => {
            // `channels` is a struct of per-type maps, not one flat map, so with
            // no filter we walk the canonical channel-type list.
            let types: Vec<String> = match channel_type {
                Some(t) => vec![t],
                None => zeroclaw_config::schema::v2::V3_CHANNEL_TYPES
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            };
            let mut types = types;
            types.sort();
            for t in types {
                if let Some(mut aliases) = config.get_map_keys(&format!("channels.{t}")) {
                    aliases.sort();
                    for a in aliases {
                        println!("{t}.{a}");
                    }
                }
            }
            Ok(())
        }
        ChannelsCommands::Create {
            channel_type,
            alias,
        } => {
            create_entry(config, &format!("channels.{channel_type}"), &alias)?;
            save(config).await
        }
        ChannelsCommands::Rename {
            channel_type,
            from,
            to,
        } => {
            rename_config(config, &AliasKind::Channel { channel_type }, &from, &to)?;
            save(config).await
        }
        ChannelsCommands::Delete {
            channel_type,
            alias,
            dry_run,
            yes,
        } => {
            let kind = AliasKind::Channel { channel_type };
            delete_config(config, &kind, &alias, dry_run, yes)?;
            if yes && !dry_run {
                save(config).await?;
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_provider_category_maps_known_and_rejects_unknown() {
        assert_eq!(
            parse_provider_category("models").unwrap(),
            ProviderCategory::Models
        );
        assert_eq!(
            parse_provider_category("tts").unwrap(),
            ProviderCategory::Tts
        );
        assert_eq!(
            parse_provider_category("transcription").unwrap(),
            ProviderCategory::Transcription
        );
        assert!(parse_provider_category("bogus").is_err());
    }

    #[test]
    fn section_path_for_each_kind() {
        assert_eq!(section_path(&AliasKind::Agent), "agents");
        assert_eq!(
            section_path(&AliasKind::Provider {
                category: ProviderCategory::Models,
                family: "anthropic".to_string(),
            }),
            "providers.models.anthropic"
        );
        assert_eq!(
            section_path(&AliasKind::Provider {
                category: ProviderCategory::Tts,
                family: "elevenlabs".to_string(),
            }),
            "providers.tts.elevenlabs"
        );
        assert_eq!(
            section_path(&AliasKind::Channel {
                channel_type: "discord".to_string(),
            }),
            "channels.discord"
        );
    }
}
