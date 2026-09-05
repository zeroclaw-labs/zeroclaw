//! CLI for alias CRUD: `zeroclaw {agents,providers,channels}
//! {create,list,rename,delete}`.

use anyhow::{Context, Result, bail};
use zeroclaw::{AgentsCommands, ChannelsCommands, ProvidersCommands};
use zeroclaw_config::alias_refs::{
    self, AliasKind, CascadeError, CascadePolicy, ProviderCategory, RenameError,
};
use zeroclaw_config::schema::Config;

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
    // The Reserved rejection is localized via Fluent like the delete/rename guards
    // below; Invalid (unknown section) keeps its pre-existing bare error.
    let created = match alias_refs::create_map_key_checked(config, section, alias) {
        Ok(created) => created,
        Err(alias_refs::CreateError::Reserved(_)) => bail!(
            "{}",
            mt(
                "cli-alias-create-reserved-default",
                "the `default` agent is reserved and cannot be created"
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

/// Print the dry-run impact (blockers + scrubs) for a delete.
fn print_impact(kind: &AliasKind, alias: &str, config: &Config) {
    let report = alias_refs::plan_delete(config, kind, alias);
    let section = section_path(kind);
    if report.blockers.is_empty() {
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
    } else {
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
        print_impact(kind, alias, config);
        return Ok(());
    }
    if !yes {
        print_impact(kind, alias, config);
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
        AgentsCommands::Rename { from, to } => {
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
            // Capture the workspace path while the `from` entry still exists
            // (custom paths are read off the entry, which the rename moves).
            let old_ws = config.agent_workspace_dir(&from);
            rename_config(config, &AliasKind::Agent, &from, &to)?;
            // Persist the config rename before the irreversible owned-state side
            // effects (workspace move + DB re-point), so a later failure can't
            // leave the config and owned state split.
            save(config).await?;
            agent_rename_owned_state(config, &from, &to, &old_ws).await
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
            if dry_run {
                print_impact(&AliasKind::Agent, &alias, config);
                return Ok(());
            }
            if !yes {
                print_impact(&AliasKind::Agent, &alias, config);
                println!(
                    "\n{}",
                    mt(
                        "cli-alias-no-changes",
                        "No changes made. Re-run with --yes to apply (or --dry-run to preview)."
                    )
                );
                return Ok(());
            }
            // Owned-state HARD gate (live ACP sessions) runs BEFORE the config
            // cascade so a refusal mutates nothing.
            agent_delete_precheck(config, &alias)?;
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

#[cfg(all(feature = "gateway", feature = "agent-runtime"))]
async fn agent_rename_owned_state(
    config: &Config,
    from: &str,
    to: &str,
    old_ws: &std::path::Path,
) -> Result<()> {
    // Move the workspace dir (default per-alias location only; a custom path is
    // alias-independent → old_ws == new_ws → skip).
    let new_ws = config.agent_workspace_dir(to);
    if old_ws != new_ws && old_ws.exists() {
        if let Some(parent) = new_ws.parent() {
            tokio::fs::create_dir_all(parent).await.ok();
        }
        if let Err(e) = tokio::fs::rename(old_ws, &new_ws).await {
            let es = e.to_string();
            eprintln!(
                "{}",
                mta(
                    "cli-alias-warn-workspace-move",
                    &[("error", es.as_str())],
                    "warning: workspace move failed: {$error}"
                )
            );
        }
    }
    let (mem, session_backend) = build_owned_state_handles(config)?;
    let report = crate::gateway::agent_owned_state::cascade_rename_agent(
        config,
        Some(&mem),
        session_backend.as_ref(),
        from,
        to,
    )
    .await;
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
async fn agent_rename_owned_state(
    _config: &Config,
    _from: &str,
    _to: &str,
    _old_ws: &std::path::Path,
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
