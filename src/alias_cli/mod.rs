//! CLI for alias CRUD: `zeroclaw {agents,providers,channels}
//! {create,list,rename,delete}`.

use anyhow::{Context, Result, bail};
use zeroclaw::{AgentsCommands, ChannelsCommands, ProvidersCommands};
use zeroclaw_config::alias_refs::{
    self, AliasKind, CascadeError, CascadePolicy, ProviderCategory, RenameError,
};
use zeroclaw_config::schema::Config;
#[cfg(feature = "agent-runtime")]
use zeroclaw_runtime::agent_rename_recovery::{
    self as rename_recovery, ConvergeOutcome, ConvergeReport, Disposition, RenameRecoveryError,
    SurfaceStores,
};

mod export;

/// Resolve a `cli-*` Fluent key for alias-CRUD CLI output. Under `agent-runtime`
/// (default + what CI/release build) this routes through Fluent; without it the
/// runtime i18n crate is absent, so the English `fallback` is used.
#[allow(unused_variables)]
fn mt(key: &str, fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string(key)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

/// `mt` with `{$name}` arguments.
#[allow(unused_variables)]
fn mta(key: &str, args: &[(&str, &str)], fallback: &str) -> String {
    #[cfg(feature = "agent-runtime")]
    {
        zeroclaw_runtime::i18n::get_required_cli_string_with_args(key, args)
    }
    #[cfg(not(feature = "agent-runtime"))]
    {
        fallback.to_string() // i18n-exempt: English fallback when Fluent (agent-runtime) is disabled
    }
}

fn parse_provider_category(category: &str) -> Result<ProviderCategory> {
    match category {
        "models" => Ok(ProviderCategory::Models),
        "tts" => Ok(ProviderCategory::Tts),
        "transcription" => Ok(ProviderCategory::Transcription),
        other => bail!(
            "{}",
            mta(
                "cli-alias-unknown-provider-category",
                &[("category", other)],
                "unknown provider category `{$category}` (expected models | tts | transcription)"
            )
        ),
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
                println!(
                    "{}",
                    mta(
                        "cli-alias-list-empty",
                        &[("section", section)],
                        "(no entries under {$section})"
                    )
                );
            } else {
                for k in keys {
                    println!("{k}");
                }
            }
        }
        None => bail!(
            "{}",
            mta(
                "cli-alias-no-such-section",
                &[("section", section)],
                "no such config section: {$section}"
            )
        ),
    }
    Ok(())
}

fn create_entry(config: &mut Config, section: &str, alias: &str) -> Result<()> {
    // Shared guarded boundary: refuses the reserved `default` agent here too (an
    // operator create surface), and delegates unchanged for every other section.
    // The Reserved rejection and the recovery-journal refusals are localized via
    // Fluent like the delete/rename guards below; Invalid (unknown section) keeps
    // its pre-existing bare error. A retired alias names the rename that must
    // finish before the alias can be reused.
    let created = match alias_refs::create_map_key_checked(config, section, alias) {
        Ok(created) => created,
        Err(alias_refs::CreateError::Reserved(_)) => bail!(
            "{}",
            mt(
                "cli-alias-create-reserved-default",
                "the `default` agent is reserved and cannot be created"
            )
        ),
        Err(alias_refs::CreateError::Retired { alias, pending_to }) => bail!(
            "{}",
            mta(
                "cli-alias-create-retired",
                &[("alias", alias.as_str()), ("to", pending_to.as_str())],
                "alias `{$alias}` is retired by an unfinished rename to `{$to}`; run `zeroclaw agents rename {$alias} {$to}` first"
            )
        ),
        Err(alias_refs::CreateError::RecoveryUnreadable(detail)) => bail!(
            "{}",
            mta(
                "cli-alias-recovery-unreadable",
                &[("error", detail.as_str())],
                "agent lifecycle recovery journal could not be read: {$error}"
            )
        ),
        Err(alias_refs::CreateError::Invalid(msg)) => return Err(anyhow::Error::msg(msg)),
    };
    if created {
        config.mark_dirty(&format!("{section}.{alias}"));
        println!(
            "{}",
            mta(
                "cli-alias-created",
                &[("section", section), ("alias", alias)],
                "created {$section}.{$alias}"
            )
        );
    } else {
        println!(
            "{}",
            mta(
                "cli-alias-exists",
                &[("section", section), ("alias", alias)],
                "{$section}.{$alias} already exists (no change)"
            )
        );
    }
    Ok(())
}

/// Print the dry-run impact (blockers + scrubs) for a delete. `refusal` is why
/// the delete would be refused before it looks at references at all, and is
/// reported as its first blocker.
fn print_impact(kind: &AliasKind, alias: &str, config: &Config, refusal: Option<&str>) {
    let report = alias_refs::plan_delete(config, kind, alias);
    let section = section_path(kind);
    if let Some(reason) = refusal {
        println!(
            "{}",
            mta(
                "cli-alias-impact-refused",
                &[
                    ("section", section.as_str()),
                    ("alias", alias),
                    ("reason", reason)
                ],
                "deleting {$section}.{$alias} is BLOCKED: {$reason}"
            )
        );
    } else if report.blockers.is_empty() {
        let count = report.scrubs.len().to_string();
        println!(
            "{}",
            mta(
                "cli-alias-impact-scrub-header",
                &[
                    ("section", section.as_str()),
                    ("alias", alias),
                    ("count", count.as_str())
                ],
                "deleting {$section}.{$alias} would scrub {$count} reference(s):"
            )
        );
    }
    if !report.blockers.is_empty() {
        let count = report.blockers.len().to_string();
        println!(
            "{}",
            mta(
                "cli-alias-impact-blocked-header",
                &[
                    ("section", section.as_str()),
                    ("alias", alias),
                    ("count", count.as_str())
                ],
                "deleting {$section}.{$alias} is BLOCKED by {$count} hard reference(s):"
            )
        );
        for b in &report.blockers {
            println!(
                "  {}",
                mta(
                    "cli-alias-impact-blocker",
                    &[("path", b.path.as_str())],
                    "✗ {$path} (hard reference)"
                )
            );
        }
    }
    for s in &report.scrubs {
        println!(
            "  {}",
            mta(
                "cli-alias-impact-scrub",
                &[("path", s.path.as_str())],
                "• {$path} (would be scrubbed)"
            )
        );
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
        print_impact(kind, alias, config, None);
        return Ok(());
    }
    if !yes {
        print_impact(kind, alias, config, None);
        println!(
            "\n{}",
            mt(
                "cli-alias-no-changes",
                "No changes made. Re-run with --yes to apply (or --dry-run to preview)."
            )
        );
        return Ok(());
    }
    apply_delete(config, kind, alias)
}

/// Apply the config-layer delete (scrub refs + remove entry) and mark the dirty
/// paths. Bails on a hard-ref refusal or a missing alias. The caller persists.
fn apply_delete(config: &mut Config, kind: &AliasKind, alias: &str) -> Result<()> {
    let section = section_path(kind);
    match alias_refs::delete_with_cascade(config, kind, alias, CascadePolicy::RefuseOnHard) {
        Ok(report) => {
            for path in report.dirty_paths() {
                config.mark_dirty(&path);
            }
            let count = report.applied.len().to_string();
            println!(
                "{}",
                mta(
                    "cli-alias-deleted",
                    &[
                        ("section", section.as_str()),
                        ("alias", alias),
                        ("count", count.as_str())
                    ],
                    "deleted {$section}.{$alias} (scrubbed {$count} reference(s))"
                )
            );
            Ok(())
        }
        Err(CascadeError::Refused(report)) => {
            let count = report.blockers.len().to_string();
            println!(
                "{}",
                mta(
                    "cli-alias-delete-refused-header",
                    &[("count", count.as_str())],
                    "refused: {$count} hard reference(s) block the delete:"
                )
            );
            for b in &report.blockers {
                println!("  ✗ {}", b.path);
            }
            bail!(
                "{}",
                mt(
                    "cli-alias-delete-refused-hint",
                    "delete refused — resolve the hard references first"
                )
            );
        }
        Err(CascadeError::NotFound(p)) => bail!(
            "{}",
            mta(
                "cli-alias-not-configured",
                &[("path", p.as_str())],
                "{$path} is not configured"
            )
        ),
        Err(e) => {
            let es = e.to_string();
            bail!(
                "{}",
                mta(
                    "cli-alias-delete-failed",
                    &[("error", es.as_str())],
                    "delete failed: {$error}"
                )
            )
        }
    }
}

/// Rename an aliased entry's config references (config-layer only).
fn rename_config(config: &mut Config, kind: &AliasKind, from: &str, to: &str) -> Result<()> {
    match alias_refs::rename_with_cascade(config, kind, from, to) {
        Ok(report) => {
            for path in &report.dirty_paths {
                config.mark_dirty(path);
            }
            let section = section_path(kind);
            let count = report.dirty_paths.len().to_string();
            println!(
                "{}",
                mta(
                    "cli-alias-renamed",
                    &[
                        ("section", section.as_str()),
                        ("from", from),
                        ("to", to),
                        ("count", count.as_str())
                    ],
                    "renamed {$section}.{$from} → {$section}.{$to} (rewrote {$count} reference path(s))"
                )
            );
            Ok(())
        }
        Err(RenameError::NotFound(p)) => bail!(
            "{}",
            mta(
                "cli-alias-not-configured",
                &[("path", p.as_str())],
                "{$path} is not configured"
            )
        ),
        Err(RenameError::InvalidName(m)) => bail!(
            "{}",
            mta(
                "cli-alias-rename-invalid",
                &[("message", m.as_str())],
                "invalid new alias: {$message}"
            )
        ),
        Err(RenameError::Reserved(a)) => bail!(
            "{}",
            mta(
                "cli-alias-rename-reserved",
                &[("alias", a.as_str())],
                "alias `{$alias}` is reserved and cannot be renamed"
            )
        ),
        Err(RenameError::PostCondition(m)) => bail!(
            "{}",
            mta(
                "cli-alias-rename-postcondition",
                &[("message", m.as_str())],
                "rename cascade post-condition failed: {$message}"
            )
        ),
    }
}

async fn save(config: &mut Config) -> Result<()> {
    Box::pin(config.save_dirty())
        .await
        .context("failed to persist config")
}

#[cfg(feature = "agent-runtime")]
pub(crate) enum AgentMutationRoute {
    Daemon(serde_json::Value),
    Offline(zeroclaw_runtime::live_config_authority::ConfigOwnershipGuard),
}

#[cfg(feature = "agent-runtime")]
pub(crate) async fn route_agent_mutation(
    config: &mut Config,
    method: &str,
    params: serde_json::Value,
) -> Result<AgentMutationRoute> {
    match zeroclaw_runtime::rpc::local::call_local(config, method, params).await {
        Ok(result) => Ok(AgentMutationRoute::Daemon(result)),
        Err(zeroclaw_runtime::rpc::local::LocalRpcCallError::Unavailable { path, source })
            if matches!(
                source.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            let ownership =
                zeroclaw_runtime::live_config_authority::ConfigOwnershipGuard::acquire(
                    &config.data_dir,
                )
                .map_err(|error| {
                    anyhow::Error::msg(format!(
                        "daemon endpoint {} is unavailable, but offline config ownership could not be acquired: {error}",
                        path.display()
                    ))
                })?;
            let expected_path = config.config_path.clone();
            let fresh = Box::pin(Config::load_or_init())
                .await
                .context("reload config after acquiring offline lifecycle ownership")?;
            anyhow::ensure!(
                fresh.config_path == expected_path,
                "config path changed while acquiring lifecycle ownership ({} -> {})",
                expected_path.display(),
                fresh.config_path.display()
            );
            *config = fresh;
            Ok(AgentMutationRoute::Offline(ownership))
        }
        Err(error) => Err(anyhow::Error::msg(format!(
            "refusing offline agent mutation because daemon coordination failed: {error}"
        ))),
    }
}

#[cfg(feature = "agent-runtime")]
fn print_daemon_create(value: serde_json::Value) -> Result<()> {
    let result: zeroclaw_runtime::rpc::types::ConfigMapKeyCreateResult =
        serde_json::from_value(value).context("decode daemon agent-create response")?;
    if result.created {
        println!(
            "{}",
            mta(
                "cli-alias-created",
                &[
                    ("section", result.path.as_str()),
                    ("alias", result.key.as_str())
                ],
                "created {$section}.{$alias}"
            )
        );
    } else {
        println!(
            "{}",
            mta(
                "cli-alias-exists",
                &[
                    ("section", result.path.as_str()),
                    ("alias", result.key.as_str())
                ],
                "{$section}.{$alias} already exists (no change)"
            )
        );
    }
    Ok(())
}

/// Report a rename the daemon ran. The daemon's warnings are the owned state
/// that has not followed the committed rename yet, so the command fails, as
/// the offline rename does, until a re-run converges it.
#[cfg(feature = "agent-runtime")]
fn print_daemon_rename(value: serde_json::Value) -> Result<()> {
    let result: zeroclaw_runtime::rpc::types::ConfigMapKeyRenameResult =
        serde_json::from_value(value).context("decode daemon agent-rename response")?;
    let count = result.rewritten.to_string();
    println!(
        "{}",
        mta(
            "cli-alias-renamed",
            &[
                ("section", result.path.as_str()),
                ("from", result.from.as_str()),
                ("to", result.to.as_str()),
                ("count", count.as_str())
            ],
            "renamed {$section}.{$from} -> {$section}.{$to} (rewrote {$count} reference path(s))"
        )
    );
    if result.warnings.is_empty() {
        return Ok(());
    }
    for warning in &result.warnings {
        eprintln!(
            "{}",
            mta(
                "cli-alias-warn",
                &[("warning", warning.as_str())],
                "warning: {$warning}"
            )
        );
    }
    bail!(
        "{}",
        mta(
            "cli-alias-rename-recovery-incomplete",
            &[("from", result.from.as_str()), ("to", result.to.as_str())],
            "agent rename did not finish; re-run `zeroclaw agents rename {$from} {$to}` to converge"
        )
    )
}

#[cfg(feature = "agent-runtime")]
fn print_daemon_delete_preview(value: serde_json::Value) -> Result<()> {
    let result: zeroclaw_runtime::rpc::types::AgentDeletePreviewResult =
        serde_json::from_value(value).context("decode daemon agent-delete preview")?;
    if result.allowed {
        let count = result.scrubs.len().to_string();
        println!(
            "{}",
            mta(
                "cli-alias-impact-scrub-header",
                &[
                    ("section", "agents"),
                    ("alias", result.alias.as_str()),
                    ("count", count.as_str())
                ],
                "deleting {$section}.{$alias} would scrub {$count} reference(s):"
            )
        );
    } else {
        let count = result.blockers.len().to_string();
        println!(
            "{}",
            mta(
                "cli-alias-impact-blocked-header",
                &[
                    ("section", "agents"),
                    ("alias", result.alias.as_str()),
                    ("count", count.as_str())
                ],
                "deleting {$section}.{$alias} is BLOCKED by {$count} hard reference(s):"
            )
        );
        for blocker in result.blockers {
            println!(
                "  {}",
                mta(
                    "cli-alias-impact-blocker",
                    &[("path", blocker.as_str())],
                    "x {$path} (hard reference)"
                )
            );
        }
    }
    for scrub in result.scrubs {
        println!(
            "  {}",
            mta(
                "cli-alias-impact-scrub",
                &[("path", scrub.as_str())],
                "- {$path} (would be scrubbed)"
            )
        );
    }
    Ok(())
}

#[cfg(feature = "agent-runtime")]
fn print_daemon_delete(value: serde_json::Value) -> Result<()> {
    let result: zeroclaw_runtime::rpc::types::AgentDeleteResult =
        serde_json::from_value(value).context("decode daemon agent-delete response")?;
    if !result.deleted {
        bail!(
            "{}",
            result
                .error
                .unwrap_or_else(|| format!("agent `{}` was not deleted", result.alias))
        );
    }
    let count = result.scrubbed.to_string();
    println!(
        "{}",
        mta(
            "cli-alias-deleted",
            &[
                ("section", "agents"),
                ("alias", result.alias.as_str()),
                ("count", count.as_str())
            ],
            "deleted {$section}.{$alias} (scrubbed {$count} reference(s))"
        )
    );
    for warning in result.warnings {
        eprintln!(
            "{}",
            mta(
                "cli-alias-warn",
                &[("warning", warning.as_str())],
                "warning: {$warning}"
            )
        );
    }
    Ok(())
}

// ── agents ──────────────────────────────────────────────────────────────────

pub async fn handle_agents(cmd: AgentsCommands, config: &mut Config) -> Result<()> {
    match cmd {
        AgentsCommands::List => list_section(config, "agents"),
        AgentsCommands::Create { alias } => {
            #[cfg(feature = "agent-runtime")]
            let _offline_ownership = match route_agent_mutation(
                config,
                "config/map-key-create",
                serde_json::json!({ "path": "agents", "key": alias }),
            )
            .await?
            {
                AgentMutationRoute::Daemon(value) => return print_daemon_create(value),
                AgentMutationRoute::Offline(ownership) => ownership,
            };
            create_entry(config, "agents", &alias)?;
            save(config).await
        }
        AgentsCommands::Export { alias, out, force } => {
            export::run(config, &alias, &out, force).await
        }
        AgentsCommands::Rename { from, to, abandon } => {
            // Abandoning changes no config, only the recovery journal, whose
            // own lock serializes it with a running daemon: it needs neither
            // the daemon route nor config ownership.
            if abandon {
                return Box::pin(abandon_agent_rename(config, &from, &to)).await;
            }
            #[cfg(feature = "agent-runtime")]
            let _offline_ownership = match route_agent_mutation(
                config,
                "config/map-key-rename",
                serde_json::json!({ "path": "agents", "from": from, "to": to }),
            )
            .await?
            {
                AgentMutationRoute::Daemon(value) => return print_daemon_rename(value),
                AgentMutationRoute::Offline(ownership) => ownership,
            };
            // Offline, the rename runs the recovery contract itself while this
            // process holds config ownership.
            Box::pin(rename_agent(config, &from, &to)).await
        }
        AgentsCommands::Delete {
            alias,
            dry_run,
            yes,
        } => {
            if alias_refs::is_reserved_agent_alias(&alias) {
                bail!(
                    "{}",
                    mt(
                        "cli-alias-delete-reserved-default",
                        "the `default` agent is reserved and cannot be deleted"
                    )
                );
            }
            #[cfg(feature = "agent-runtime")]
            let _offline_ownership = {
                let method = if dry_run || !yes {
                    "agents/delete-preview"
                } else {
                    "agents/delete"
                };
                match route_agent_mutation(config, method, serde_json::json!({ "alias": alias }))
                    .await?
                {
                    AgentMutationRoute::Daemon(value) => {
                        if dry_run {
                            return print_daemon_delete_preview(value);
                        }
                        if !yes {
                            print_daemon_delete_preview(value)?;
                            println!(
                                "\n{}",
                                mt(
                                    "cli-alias-no-changes",
                                    "No changes made. Re-run with --yes to apply (or --dry-run to preview)."
                                )
                            );
                            return Ok(());
                        }
                        return print_daemon_delete(value);
                    }
                    AgentMutationRoute::Offline(ownership) => ownership,
                }
            };
            if dry_run || !yes {
                // The preview reports the refusal the delete itself would make
                // for an alias an unfinished rename still owns.
                let refusal = agent_delete_recovery_refusal(config, &alias).await;
                print_impact(&AliasKind::Agent, &alias, config, refusal.as_deref());
                if !dry_run {
                    println!(
                        "\n{}",
                        mt(
                            "cli-alias-no-changes",
                            "No changes made. Re-run with --yes to apply (or --dry-run to preview)."
                        )
                    );
                }
                return Ok(());
            }
            // Owned-state HARD gate (live ACP sessions) runs BEFORE the config
            // cascade so a refusal mutates nothing.
            agent_delete_precheck(config, &alias)?;
            // An unfinished rename still owes state moves out of its old alias
            // and into its target. Deleting either would strand that state, so
            // both refuse until the rename converges.
            if let Some(refusal) = agent_delete_recovery_refusal(config, &alias).await {
                return Err(anyhow::Error::msg(refusal));
            }
            // Resolve the workspace dir while the entry still exists (a custom
            // `workspace.path` is read off it), then apply + PERSIST the config
            // change before any irreversible owned-state side effects — so a
            // later failure can't leave the config and owned state split.
            let workspace = config.agent_workspace_dir(&alias);
            let owned_state_handles = build_owned_state_handles(config)?;
            apply_delete(config, &AliasKind::Agent, &alias)?;
            save(config).await?;
            agent_delete_owned_state(config, &alias, &workspace, owned_state_handles).await
        }
    }
}

/// Rename an agent through the recovery contract every rename surface shares.
///
/// A fresh rename is recorded before its config commit, and the state kept
/// under the old alias follows once the commit lands. Re-running a rename that
/// did not finish resumes it instead of reporting the old alias missing, and
/// the command fails until every follower has converged.
#[cfg(feature = "agent-runtime")]
async fn rename_agent(config: &mut Config, from: &str, to: &str) -> Result<()> {
    // The CLI holds no store handles: the contract opens each follower's store
    // from the config and what exists on disk.
    let stores = SurfaceStores::none();
    match rename_recovery::resolve(config, from, to, &stores)
        .await
        .map_err(rename_refused)?
    {
        Disposition::Fresh => {
            let armed = rename_recovery::arm(config, from, to)
                .await
                .map_err(rename_refused)?;
            // Commit on a working copy, so `config` shows the rename only once
            // the save has landed.
            let mut working = config.clone();
            let committed = match rename_config(&mut working, &AliasKind::Agent, from, to) {
                Ok(()) => save(&mut working).await,
                Err(e) => Err(e),
            };
            if let Err(e) = committed {
                rename_recovery::abandon(config, armed).await;
                return Err(e);
            }
            *config = working;
            rename_recovery::acknowledge_commit(config, armed).await;
        }
        Disposition::Resume => println!(
            "{}",
            mta(
                "cli-alias-rename-resuming",
                &[("from", from), ("to", to)],
                "resuming the unfinished rename of {$from} to {$to}"
            )
        ),
    }

    match rename_recovery::converge(config, from, to, &stores).await {
        Ok(ConvergeOutcome::Converged(report)) => {
            print_repointed(&report);
            Ok(())
        }
        Ok(ConvergeOutcome::Incomplete {
            report,
            outstanding,
        }) => {
            print_repointed(&report);
            for issue in &outstanding {
                let warning = issue.to_string();
                eprintln!(
                    "{}",
                    mta(
                        "cli-alias-warn",
                        &[("warning", warning.as_str())],
                        "warning: {$warning}"
                    )
                );
            }
            bail!(
                "{}",
                mta(
                    "cli-alias-rename-recovery-incomplete",
                    &[("from", from), ("to", to)],
                    "agent rename did not finish; re-run `zeroclaw agents rename {$from} {$to}` to converge"
                )
            )
        }
        Err(e) => Err(rename_recovery_failed(&e)),
    }
}

/// Without the agent runtime there is no recovery contract: the config rename
/// is saved and the operator is told the owned state did not follow.
#[cfg(not(feature = "agent-runtime"))]
async fn rename_agent(config: &mut Config, from: &str, to: &str) -> Result<()> {
    rename_config(config, &AliasKind::Agent, from, to)?;
    save(config).await?;
    warn_agent_owned_state();
    Ok(())
}

/// Report what one converge moved. ACP counts the sessions re-attributed to
/// the new alias; a session whose saved working directory moved with the
/// workspace is the same session, so it is not counted twice.
#[cfg(feature = "agent-runtime")]
fn print_repointed(report: &ConvergeReport) {
    let memory = report.memory_rows.to_string();
    let cron = report.cron_jobs.to_string();
    let acp = report.acp_sessions.to_string();
    let sessions = report.sessions_repointed.to_string();
    println!(
        "{}",
        mta(
            "cli-alias-owned-repointed",
            &[
                ("memory", memory.as_str()),
                ("cron", cron.as_str()),
                ("acp", acp.as_str()),
                ("sessions", sessions.as_str())
            ],
            "owned-state re-pointed: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions}"
        )
    );
}

/// Localize why the rename recovery contract refused a rename. A refusal an
/// unfinished rename decided names the rename to re-run, and a store or
/// journal that could not be read never reads as a missing alias: the same
/// rename can be retried once it is readable.
#[cfg(feature = "agent-runtime")]
fn rename_refused(error: RenameRecoveryError) -> anyhow::Error {
    let message = match &error {
        RenameRecoveryError::NotConfigured { alias } => {
            let path = format!("{}.{alias}", section_path(&AliasKind::Agent));
            mta(
                "cli-alias-not-configured",
                &[("path", path.as_str())],
                "{$path} is not configured",
            )
        }
        // Either alias can be the malformed one, so the text names it instead
        // of assuming the new alias like the config-only rename does.
        RenameRecoveryError::InvalidAlias { alias, reason } => mta(
            "cli-alias-rename-agent-invalid",
            &[("alias", alias.as_str()), ("message", reason.as_str())],
            "invalid agent alias `{$alias}`: {$message}",
        ),
        RenameRecoveryError::ReservedAlias { alias } => mta(
            "cli-alias-rename-reserved",
            &[("alias", alias.as_str())],
            "alias `{$alias}` is reserved and cannot be renamed",
        ),
        RenameRecoveryError::AliasRetired { alias, pending_to } => mta(
            "cli-alias-rename-retired",
            &[("alias", alias.as_str()), ("to", pending_to.as_str())],
            "alias `{$alias}` is retired by an unfinished rename to `{$to}`; run `zeroclaw agents rename {$alias} {$to}` first",
        ),
        RenameRecoveryError::RecoveryPending { from, to } => mta(
            "cli-alias-rename-pending-target",
            &[("from", from.as_str()), ("to", to.as_str())],
            "agent `{$to}` is the target of an unfinished rename from `{$from}`; run `zeroclaw agents rename {$from} {$to}` first",
        ),
        RenameRecoveryError::SourceReconfigured { from, to } => mta(
            "cli-alias-rename-source-reconfigured",
            &[("from", from.as_str()), ("to", to.as_str())],
            "agent `{$from}` is configured again while its rename to `{$to}` is unfinished; remove `[agents.{$from}]` from the config by hand, or run `zeroclaw agents rename {$from} {$to} --abandon`",
        ),
        RenameRecoveryError::Unreadable { .. }
        | RenameRecoveryError::Busy { .. }
        | RenameRecoveryError::Persist { .. } => return rename_recovery_failed(&error),
    };
    anyhow::Error::msg(message)
}

/// A recovery step that failed rather than refused: a store or the recovery
/// journal could not be read or written, or another process holds the journal.
#[cfg(feature = "agent-runtime")]
fn rename_recovery_failed(error: &RenameRecoveryError) -> anyhow::Error {
    let detail = error.to_string();
    anyhow::Error::msg(mta(
        "cli-alias-rename-recovery-error",
        &[("error", detail.as_str())],
        "agent rename recovery failed: {$error}",
    ))
}

/// Why deleting `alias` is refused because of an unfinished rename, localized:
/// the rename retired `alias`, or is still converging into it. A recovery
/// journal that cannot be read refuses too. The delete and its preview both
/// report this.
#[cfg(feature = "agent-runtime")]
async fn agent_delete_recovery_refusal(config: &Config, alias: &str) -> Option<String> {
    let checked = match rename_recovery::ensure_alias_not_retired(config, alias).await {
        Ok(()) => rename_recovery::ensure_not_pending_target(config, alias).await,
        Err(e) => Err(e),
    };
    let refusal = match checked.err()? {
        RenameRecoveryError::AliasRetired { alias, pending_to } => mta(
            "cli-alias-delete-retired",
            &[("alias", alias.as_str()), ("to", pending_to.as_str())],
            "alias `{$alias}` is retired by an unfinished rename to `{$to}`; run `zeroclaw agents rename {$alias} {$to}` before deleting anything",
        ),
        e @ RenameRecoveryError::RecoveryPending { .. } => rename_refused(e).to_string(),
        e => rename_recovery_failed(&e).to_string(),
    };
    Some(refusal)
}

/// Without the agent runtime the delete runs no recovery guard, so nothing
/// refuses it here.
#[cfg(not(feature = "agent-runtime"))]
async fn agent_delete_recovery_refusal(_config: &Config, _alias: &str) -> Option<String> {
    None
}

/// Drop the recovery record of the unfinished rename of `from` to `to`
/// without moving anything, after listing the state still kept under `from`,
/// which an agent created under it adopts.
#[cfg(feature = "agent-runtime")]
async fn abandon_agent_rename(config: &Config, from: &str, to: &str) -> Result<()> {
    let abandoned = match rename_recovery::abandon_rename(config, from, to).await {
        Ok(abandoned) => abandoned,
        // For an abandon, nothing to rename means no record to drop.
        Err(RenameRecoveryError::NotConfigured { .. }) => bail!(
            "{}",
            mta(
                "cli-alias-rename-abandon-none",
                &[("from", from), ("to", to)],
                "there is no unfinished rename of `{$from}` to `{$to}` to abandon"
            )
        ),
        Err(e) => return Err(rename_refused(e)),
    };
    for issue in &abandoned.residue {
        let warning = issue.to_string();
        eprintln!(
            "{}",
            mta(
                "cli-alias-warn",
                &[("warning", warning.as_str())],
                "warning: {$warning}"
            )
        );
    }
    println!(
        "{}",
        mta(
            "cli-alias-rename-abandoned",
            &[("from", from), ("to", to)],
            "dropped the unfinished rename of {$from} to {$to}; `{$from}` can be created again and will adopt any state still listed above"
        )
    );
    Ok(())
}

/// Without the agent runtime there is no recovery contract to abandon a
/// rename through.
#[cfg(not(feature = "agent-runtime"))]
async fn abandon_agent_rename(_config: &Config, _from: &str, _to: &str) -> Result<()> {
    bail!(
        "{}",
        mt(
            "cli-alias-rename-abandon-unavailable",
            "abandoning an unfinished agent rename needs a build with the agent runtime"
        )
    )
}

/// Memory + optional session-backend handles opened from `data_dir` for the
/// owned-state cascade.
#[cfg(all(feature = "gateway", feature = "agent-runtime"))]
type OwnedStateHandles = (
    std::sync::Arc<dyn zeroclaw_memory::Memory>,
    Option<std::sync::Arc<dyn zeroclaw_infra::session_backend::SessionBackend>>,
);

#[cfg(not(all(feature = "gateway", feature = "agent-runtime")))]
type OwnedStateHandles = ();

#[cfg(all(feature = "gateway", feature = "agent-runtime"))]
fn build_owned_state_handles(config: &Config) -> Result<OwnedStateHandles> {
    use std::sync::Arc;
    let mem: Arc<dyn zeroclaw_memory::Memory> = if config.agents.is_empty() {
        Arc::new(zeroclaw_memory::NoneMemory::new("none"))
    } else {
        Arc::from(
            zeroclaw_memory::create_memory_from_config(config, None)
                .context("open memory backend for the owned-state cascade")?,
        )
    };
    let session_backend = if config.gateway.session_persistence {
        Some(
            zeroclaw_infra::make_session_backend(
                &config.data_dir,
                &config.channels.session_backend,
            )
            .context("open session backend for the owned-state cascade")?,
        )
    } else {
        None
    };
    Ok((mem, session_backend))
}

#[cfg(not(all(feature = "gateway", feature = "agent-runtime")))]
fn build_owned_state_handles(_config: &Config) -> Result<OwnedStateHandles> {
    Ok(())
}

#[cfg(all(feature = "gateway", feature = "agent-runtime"))]
fn agent_delete_precheck(config: &Config, alias: &str) -> Result<()> {
    // Fail closed: refuse if live ACP sessions exist, or if the store can't be
    // read to verify (mirrors the gateway delete gate).
    let live = crate::gateway::agent_owned_state::live_acp_session_count(config, alias)
        .context("could not verify live ACP sessions")?;
    if live > 0 {
        let count = live.to_string();
        bail!(
            "{}",
            mta(
                "cli-alias-live-acp-sessions",
                &[("count", count.as_str()), ("alias", alias)],
                "{$count} live ACP session(s) for `{$alias}` — end them first"
            )
        );
    }
    Ok(())
}

#[cfg(not(all(feature = "gateway", feature = "agent-runtime")))]
fn agent_delete_precheck(_config: &Config, _alias: &str) -> Result<()> {
    Ok(())
}

#[cfg(all(feature = "gateway", feature = "agent-runtime"))]
async fn agent_delete_owned_state(
    config: &Config,
    alias: &str,
    workspace: &std::path::Path,
    (mem, session_backend): OwnedStateHandles,
) -> Result<()> {
    let archive =
        zeroclaw_runtime::agent_lifecycle::archive_agent_workspace(config, alias, workspace).await;
    for warning in archive.warnings {
        eprintln!(
            "{}",
            mta(
                "cli-alias-warn-workspace-archive",
                &[("error", warning.as_str())],
                "warning: workspace archive failed: {$error}"
            )
        );
    }
    let archive_dir = archive.archive_dir;
    let report = crate::gateway::agent_owned_state::cascade_owned_state(
        config,
        &mem,
        session_backend.as_ref(),
        alias,
        &archive_dir,
    )
    .await;
    let memory = report.memory_purged.to_string();
    let cron = report.cron_removed.to_string();
    let acp = report.acp_removed.to_string();
    let sessions = report.sessions_cleared.to_string();
    let archive = archive_dir.display().to_string();
    println!(
        "{}",
        mta(
            "cli-alias-owned-cascaded",
            &[
                ("memory", memory.as_str()),
                ("cron", cron.as_str()),
                ("acp", acp.as_str()),
                ("sessions", sessions.as_str()),
                ("archive", archive.as_str())
            ],
            "owned-state cascaded: memory {$memory} · cron {$cron} · acp {$acp} · sessions {$sessions} → {$archive}"
        )
    );
    for w in &report.warnings {
        eprintln!(
            "{}",
            mta(
                "cli-alias-warn",
                &[("warning", w.as_str())],
                "warning: {$warning}"
            )
        );
    }
    Ok(())
}

#[cfg(not(all(feature = "gateway", feature = "agent-runtime")))]
async fn agent_delete_owned_state(
    _config: &Config,
    _alias: &str,
    _workspace: &std::path::Path,
    _owned_state_handles: (),
) -> Result<()> {
    warn_agent_owned_state();
    Ok(())
}

#[cfg(not(all(feature = "gateway", feature = "agent-runtime")))]
fn warn_agent_owned_state() {
    eprintln!(
        "{}",
        mt(
            "cli-alias-owned-state-unavailable",
            "note: config references were updated, but the agent's owned state \
             (memory rows, workspace dir, cron/acp/session rows) was NOT cascaded \
             by this CLI yet — use the gateway API for the full owned-state cascade."
        )
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

    #[cfg(feature = "agent-runtime")]
    #[test]
    fn rename_refusals_name_the_rename_that_must_finish_first() {
        let retired = rename_refused(RenameRecoveryError::AliasRetired {
            alias: "scout".to_string(),
            pending_to: "ranger".to_string(),
        })
        .to_string();
        let pending = rename_refused(RenameRecoveryError::RecoveryPending {
            from: "scout".to_string(),
            to: "ranger".to_string(),
        })
        .to_string();
        for message in [&retired, &pending] {
            assert!(
                message.contains("zeroclaw agents rename scout ranger"),
                "{message}"
            );
            assert!(!message.contains("{cli-"), "missing Fluent key: {message}");
        }
        assert_ne!(retired, pending);
    }

    #[cfg(feature = "agent-runtime")]
    #[test]
    fn a_rename_recovery_failure_never_reads_as_a_missing_alias() {
        let missing = rename_refused(RenameRecoveryError::NotConfigured {
            alias: "scout".to_string(),
        })
        .to_string();
        assert!(missing.contains("agents.scout"), "{missing}");

        for error in [
            RenameRecoveryError::Unreadable {
                store: "cron".to_string(),
                detail: "disk I/O error".to_string(),
            },
            RenameRecoveryError::Busy {
                detail: "journal locked".to_string(),
            },
            RenameRecoveryError::Persist {
                detail: "read-only file system".to_string(),
            },
        ] {
            let detail = error.to_string();
            let message = rename_refused(error).to_string();
            assert!(message.contains(&detail), "{message}");
            assert!(!message.contains("{cli-"), "missing Fluent key: {message}");
            assert!(!message.contains("is not configured"), "{message}");
            assert_ne!(message, missing);
        }
    }

    #[cfg(feature = "agent-runtime")]
    #[test]
    fn a_source_configured_again_names_both_ways_out() {
        let message = rename_refused(RenameRecoveryError::SourceReconfigured {
            from: "scout".to_string(),
            to: "ranger".to_string(),
        })
        .to_string();
        assert!(message.contains("[agents.scout]"), "{message}");
        assert!(
            message.contains("zeroclaw agents rename scout ranger --abandon"),
            "{message}"
        );
        assert!(!message.contains("{cli-"), "missing Fluent key: {message}");
    }

    #[cfg(feature = "agent-runtime")]
    #[test]
    fn a_daemon_rename_that_reports_unfinished_work_fails() {
        let result = |warnings: &[&str]| {
            serde_json::json!({
                "path": "agents",
                "from": "scout",
                "to": "ranger",
                "renamed": true,
                "rewritten": 1,
                "warnings": warnings,
            })
        };
        assert!(print_daemon_rename(result(&[])).is_ok());

        let error = print_daemon_rename(result(&[
            "the old default workspace of `scout` still exists at /tmp/scout",
        ]))
        .expect_err("unfinished rename work fails the command")
        .to_string();
        assert!(
            error.contains("zeroclaw agents rename scout ranger"),
            "{error}"
        );
        assert!(!error.contains("{cli-"), "missing Fluent key: {error}");
    }

    #[cfg(feature = "agent-runtime")]
    #[tokio::test]
    async fn agent_mutation_fails_closed_when_owner_has_no_rpc_endpoint() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            config_path: temp.path().join("config.toml"),
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        let _owner = zeroclaw_runtime::LiveConfigAuthority::new_owned(config.clone()).unwrap();

        let error = route_agent_mutation(
            &mut config,
            "config/map-key-create",
            serde_json::json!({ "path": "agents", "key": "blocked" }),
        )
        .await
        .err()
        .expect("offline mutation must not bypass a live owner");

        assert!(
            error
                .to_string()
                .contains("ownership could not be acquired")
        );
        assert!(!config.agents.contains_key("blocked"));
    }

    #[cfg(all(feature = "gateway", feature = "agent-runtime"))]
    #[tokio::test]
    async fn final_agent_delete_retains_the_predelete_memory_backend_for_cleanup() {
        use std::sync::Arc;
        use zeroclaw_api::memory_traits::{Memory, MemoryCategory};

        let temp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            config_path: temp.path().join("config.toml"),
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        config.memory.backend = "sqlite".to_string();
        config.agents.insert(
            "victim".to_string(),
            zeroclaw_config::schema::AliasedAgentConfig::default(),
        );

        let workspace = config.agent_workspace_dir("victim");
        let handles = build_owned_state_handles(&config).unwrap();
        let retained_memory = Arc::clone(&handles.0);
        let agent_id = retained_memory.ensure_agent_uuid("victim").await.unwrap();
        retained_memory
            .store_with_agent(
                "owned-row",
                "must be purged",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&agent_id),
            )
            .await
            .unwrap();
        assert_eq!(retained_memory.count().await.unwrap(), 1);

        apply_delete(&mut config, &AliasKind::Agent, "victim").unwrap();
        assert!(config.agents.is_empty());
        agent_delete_owned_state(&config, "victim", &workspace, handles)
            .await
            .unwrap();

        assert_eq!(
            retained_memory.count().await.unwrap(),
            0,
            "deleting the final configured agent must still purge its durable memory rows"
        );
    }
}
