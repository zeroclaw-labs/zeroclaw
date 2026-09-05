//! Agent rename/delete **owned-state** cascades: the non-config half of
//! changing an agent alias lifecycle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use tokio::io::AsyncWriteExt;
use zeroclaw_api::attribution::{Attributable, MemoryKind, Role};
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

pub fn live_acp_session_count(config: &Config, alias: &str) -> anyhow::Result<usize> {
    let store = AcpSessionStore::new(&config.data_dir)
        .context("open ACP session store to verify live sessions")?;
    store
        .count_live_sessions_by_agent(alias)
        .context("count live ACP sessions for agent")
}

fn resolve_memory_for_owned_state(
    config: &Config,
    memory: Option<&Arc<dyn Memory>>,
) -> anyhow::Result<Option<Arc<dyn Memory>>> {
    // An already-open handle may still contain rows after a live config toggle;
    // preserve the pre-existing cleanup behavior instead of stranding those
    // rows when the currently selected backend is `none`.
    let configured_backend = zeroclaw_memory::backend_kind_from_dotted(&config.memory.backend);
    let configured_kind = zeroclaw_memory::classify_memory_backend(&configured_backend);

    if let Some(memory) = memory
        && (!matches!(memory.role(), Role::Memory(MemoryKind::None))
            || matches!(configured_kind, zeroclaw_memory::MemoryBackendKind::None))
    {
        return Ok(Some(Arc::clone(memory)));
    }

    // The gateway deliberately falls back to `NoneMemory` when its configured
    // durable backend cannot be opened. That placeholder keeps unrelated HTTP
    // surfaces alive, but it is not evidence that owned memory is empty. When
    // config still expects persistence, ignore the placeholder and reopen the
    // canonical configured backend so deletion either cleans it or fails toward
    // a later retry.
    if matches!(configured_kind, zeroclaw_memory::MemoryBackendKind::None) {
        return Ok(None);
    }

    zeroclaw_memory::create_memory_from_config(config, None)
        .map(Arc::from)
        .map(Some)
        .context("open configured memory backend for owned-state recovery")
}

fn resolve_session_backend_for_owned_state(
    config: &Config,
    session_backend: Option<&Arc<dyn SessionBackend>>,
) -> std::io::Result<Option<Arc<dyn SessionBackend>>> {
    // As with memory, an existing handle is authoritative evidence that a
    // durable store is reachable and may contain stale attribution from before
    // a live config toggle.
    if let Some(session_backend) = session_backend {
        return Ok(Some(Arc::clone(session_backend)));
    }
    // Gateway WebSocket and channel histories share this backend. Only an
    // absent handle with both producers disabled means there is no configured
    // store to recover; otherwise absence means the expected store is
    // unavailable and must fail toward another retry.
    if !config.gateway.session_persistence && !config.channels.session_persistence {
        return Ok(None);
    }

    zeroclaw_infra::make_session_backend(&config.data_dir, &config.channels.session_backend)
        .map(Some)
}

#[derive(Debug, Clone, Copy)]
enum CommittedLifecycle {
    Delete,
    Rename,
}

/// Does durable owned state still exist for `alias` after its config mutation
/// is already committed?
///
/// Delete and rename share every physical store probe, but memory deliberately
/// has distinct canonical predicates: delete recovery only needs rows that
/// require purging, while rename recovery must also detect the stable agent
/// identity row moved by `Memory::rename_agent` when it owns zero memories.
/// Both modes fail toward residue when state cannot be inspected.
async fn committed_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    lifecycle: CommittedLifecycle,
) -> bool {
    match config.agent_workspace_dir(alias).try_exists() {
        Ok(true) | Err(_) => return true,
        Ok(false) => {}
    }

    if crate::cron::list_jobs_by_agent(config, alias)
        .map(|jobs| !jobs.is_empty())
        .unwrap_or(true)
    {
        return true;
    }

    match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => {
            if store
                .list_sessions_by_agent(alias)
                .map(|sessions| !sessions.is_empty())
                .unwrap_or(true)
            {
                return true;
            }
        }
        Err(_) => return true,
    }

    match resolve_memory_for_owned_state(config, mem) {
        Ok(Some(mem)) => {
            let residue = match lifecycle {
                CommittedLifecycle::Delete => mem
                    .export_agent(alias)
                    .await
                    .map_or(true, |rows| !rows.is_empty()),
                CommittedLifecycle::Rename => {
                    mem.count_agent(alias).await.map_or(true, |count| count > 0)
                }
            };
            if residue {
                return true;
            }
        }
        Err(_) => return true,
        Ok(_) => {}
    }

    let knowledge_path = config.knowledge.resolved_db_path();
    match knowledge_path.try_exists() {
        Ok(true) => match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &knowledge_path,
            config.knowledge.max_nodes,
        ) {
            Ok(graph) if graph.count_owner(alias).unwrap_or(1) > 0 => return true,
            Err(_) => return true,
            Ok(_) => {}
        },
        Err(_) => return true,
        Ok(false) => {}
    }

    match resolve_session_backend_for_owned_state(config, session_backend) {
        Ok(Some(backend)) if backend.count_agent_attribution(alias).unwrap_or(1) > 0 => {
            return true;
        }
        Err(_) => return true,
        Ok(_) => {}
    }

    false
}

/// Does purgeable state still exist for `alias` after its config entry is
/// already gone?
///
/// This is the shared **committed-delete recovery** contract. Every supported
/// alias lifecycle surface (gateway, CLI, RPC) persists the removal of
/// `agents.<alias>` before running the owned-state cascade. A retry re-enters
/// that cascade while owned rows or an unreadable store remain.
pub async fn committed_delete_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
) -> bool {
    committed_residue_exists(
        config,
        mem,
        session_backend,
        alias,
        CommittedLifecycle::Delete,
    )
    .await
}

/// Does state still require re-pointing after an agent rename was committed?
///
/// Unlike delete recovery, memory checks the alias identity row through
/// `Memory::count_agent`, exactly mirroring `Memory::rename_agent` even when
/// the old alias owns no memory entries.
pub async fn committed_rename_residue_exists(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
) -> bool {
    committed_residue_exists(
        config,
        mem,
        session_backend,
        alias,
        CommittedLifecycle::Rename,
    )
    .await
}

/// Durable archive location and any workspace-archive failures that must be
/// surfaced alongside the owned-store cascade.
#[derive(Debug)]
pub struct AgentDeletionArchive {
    pub path: PathBuf,
    pub warnings: Vec<String>,
}

/// Create the canonical agent-deletion archive and move its workspace into it.
pub async fn archive_agent_workspace(
    config: &Config,
    alias: &str,
    workspace: &Path,
) -> AgentDeletionArchive {
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let archive_dir = config
        .data_dir
        .join("agents")
        .join("_deleted")
        .join(format!("{alias}-{ts}"));
    let mut warnings = Vec::new();
    if let Err(error) = tokio::fs::create_dir_all(&archive_dir).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({
                    "agent": alias,
                    "archive": archive_dir.display().to_string(),
                    "error": error.to_string(),
                })),
            "agent delete: archive directory creation failed"
        );
        warnings.push(format!(
            "archive directory creation failed ({}): {error}",
            archive_dir.display()
        ));
    }
    match workspace.try_exists() {
        Ok(true) => {
            let destination = archive_dir.join("workspace");
            if let Err(error) = tokio::fs::rename(workspace, &destination).await {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(::serde_json::json!({
                            "agent": alias,
                            "from": workspace.display().to_string(),
                            "to": destination.display().to_string(),
                            "error": error.to_string(),
                        })),
                    "agent delete: workspace archive failed"
                );
                warnings.push(format!(
                    "workspace archive failed ({} -> {}): {error}",
                    workspace.display(),
                    destination.display()
                ));
            }
        }
        Ok(false) => {}
        Err(error) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({
                        "agent": alias,
                        "workspace": workspace.display().to_string(),
                        "error": error.to_string(),
                    })),
                "agent delete: workspace inspection failed"
            );
            warnings.push(format!(
                "workspace inspection failed ({}): {error}",
                workspace.display()
            ));
        }
    }

    AgentDeletionArchive {
        path: archive_dir,
        warnings,
    }
}

/// What the owned-state cascade removed.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct OwnedStateReport {
    pub memory_purged: usize,
    pub knowledge_purged: usize,
    #[serde(default)]
    pub knowledge_foreign_edges_purged: usize,
    pub cron_removed: usize,
    pub acp_removed: usize,
    pub sessions_cleared: usize,
    pub archived_to: Option<String>,
    /// Surfaced failures (export / purge / delete errors). Non-empty means part
    /// of the cascade did NOT complete — those rows were not silently treated as
    /// removed. The handler logs these; nothing is masked as success.
    pub warnings: Vec<String>,
}

async fn write_json(path: &Path, bytes: Vec<u8>) -> anyhow::Result<()> {
    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(path)
        .await
        .with_context(|| format!("open archive file {}", path.display()))?;
    file.write_all(&bytes)
        .await
        .with_context(|| format!("write archive file {}", path.display()))?;
    file.sync_all()
        .await
        .with_context(|| format!("sync archive file {}", path.display()))?;
    drop(file);

    // Persist the directory entry as well as the file contents on platforms
    // that support syncing directories. A successful return is the deletion
    // gate for knowledge rows, so it must mean the recovery path is durable.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        let parent_dir = tokio::fs::File::open(parent)
            .await
            .with_context(|| format!("open archive directory {}", parent.display()))?;
        parent_dir
            .sync_all()
            .await
            .with_context(|| format!("sync archive directory {}", parent.display()))?;
    }

    Ok(())
}

fn archive_warning(kind: &str, err: &anyhow::Error) -> String {
    format!("{kind} archive: {err}")
}

fn knowledge_purge_skipped_warning(err: &anyhow::Error) -> String {
    let error = err.to_string();
    crate::i18n::get_required_cli_string_with_args(
        "cli-alias-knowledge-purge-skipped",
        &[("error", error.as_str())],
    )
}

pub async fn cascade_owned_state(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
) -> OwnedStateReport {
    let cascade_dir = archive_dir.join("cascade");
    let mut warnings: Vec<String> = Vec::new();
    if let Err(err) = tokio::fs::create_dir_all(&cascade_dir).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": cascade_dir.display().to_string(), "err": err.to_string()})),
            "owned-state cascade: failed to create archive directory"
        );
        warnings.push(format!(
            "cascade archive directory creation failed ({}): {err}",
            cascade_dir.display()
        ));
    }

    let resolved_memory = match resolve_memory_for_owned_state(config, mem) {
        Ok(memory) => memory,
        Err(error) => {
            warnings.push(format!("memory backend unavailable: {error}"));
            None
        }
    };

    // ── memory: export → archive → purge. Failures are SURFACED in `warnings`,
    // not masked as 0 (markdown/none have no DB rows — their memory lives in the
    // archived workspace — but a real backend error must stay visible). ────────
    let memory_purged = if let Some(mem) = resolved_memory.as_ref() {
        match mem
            .export_agent(alias)
            .await
            .context("export owned memory")
            .and_then(|rows| serde_json::to_vec_pretty(&rows).context("serialize memory export"))
        {
            Ok(bytes) => match write_json(&cascade_dir.join("memory.json"), bytes).await {
                Ok(()) => match mem.purge_agent(alias).await {
                    Ok(n) => n,
                    Err(e) => {
                        warnings.push(format!("memory purge: {e}"));
                        0
                    }
                },
                Err(err) => {
                    warnings.push(archive_warning("memory", &err));
                    0
                }
            },
            Err(err) => {
                warnings.push(archive_warning("memory", &err));
                0
            }
        }
    } else {
        0
    };

    // ── knowledge graph: export → archive → purge. The graph owns durable
    // alias attribution, so it participates in the same canonical lifecycle
    // cascade as memory/session state. Avoid creating an empty DB when the
    // operator has never enabled knowledge.
    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_purge = match knowledge_path.try_exists() {
        Ok(true) => match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &knowledge_path,
            config.knowledge.max_nodes,
        ) {
            Ok(graph) => match graph
                .export_owner(alias)
                .context("export owned knowledge")
                .and_then(|rows| {
                    serde_json::to_vec_pretty(&rows).context("serialize owned knowledge export")
                }) {
                Ok(bytes) => match write_json(&cascade_dir.join("knowledge.json"), bytes).await {
                    Ok(()) => match graph.purge_owner_with_report(alias) {
                        Ok(report) => report,
                        Err(e) => {
                            warnings.push(format!("knowledge purge: {e}"));
                            Default::default()
                        }
                    },
                    Err(err) => {
                        warnings.push(knowledge_purge_skipped_warning(&err));
                        Default::default()
                    }
                },
                Err(err) => {
                    warnings.push(knowledge_purge_skipped_warning(&err));
                    Default::default()
                }
            },
            Err(e) => {
                warnings.push(format!("knowledge graph open: {e}"));
                Default::default()
            }
        },
        Ok(false) => Default::default(),
        Err(error) => {
            warnings.push(format!(
                "knowledge graph inspection ({}): {error}",
                knowledge_path.display()
            ));
            Default::default()
        }
    };

    // ── cron: list → archive → remove (cron_runs cascade off job_id) ─────────
    let cron_removed = match crate::cron::list_jobs_by_agent(config, alias) {
        Ok(jobs) => match serde_json::to_vec_pretty(&jobs).context("serialize cron export") {
            Ok(bytes) => match write_json(&cascade_dir.join("cron.json"), bytes).await {
                Ok(()) => match crate::cron::remove_jobs_by_agent(config, alias) {
                    Ok(n) => n,
                    Err(error) => {
                        warnings.push(format!("cron remove: {error}"));
                        0
                    }
                },
                Err(error) => {
                    warnings.push(archive_warning("cron", &error));
                    0
                }
            },
            Err(error) => {
                warnings.push(archive_warning("cron", &error));
                0
            }
        },
        Err(e) => {
            warnings.push(format!("cron list: {e}"));
            0
        }
    };

    // ── acp: list → archive → delete (only killed sessions remain) ───────────
    let mut acp_removed = 0;
    match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => match store.list_sessions_by_agent(alias) {
            Ok(sessions) => {
                // AcpSessionSummary isn't Serialize; hand-map the fields we keep.
                let json: Vec<serde_json::Value> = sessions
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "session_uuid": s.session_uuid,
                            "agent_alias": s.agent_alias,
                            "workspace_dir": s.workspace_dir,
                            "token_count": s.token_count,
                            "message_count": s.message_count,
                            "created_at": s.created_at.to_rfc3339(),
                            "last_activity": s.last_activity.to_rfc3339(),
                        })
                    })
                    .collect();
                match serde_json::to_vec_pretty(&json).context("serialize ACP export") {
                    Ok(bytes) => match write_json(&cascade_dir.join("acp.json"), bytes).await {
                        Ok(()) => match store.delete_sessions_by_agent(alias) {
                            Ok(n) => acp_removed = n,
                            Err(error) => warnings.push(format!("acp delete: {error}")),
                        },
                        Err(error) => warnings.push(archive_warning("ACP", &error)),
                    },
                    Err(error) => warnings.push(archive_warning("ACP", &error)),
                }
            }
            Err(error) => warnings.push(format!("acp list: {error}")),
        },
        Err(e) => warnings.push(format!("acp store open: {e}")),
    }

    // ── session metadata: clear the stale agent attribution (keep the convo) ─
    let resolved_session_backend =
        match resolve_session_backend_for_owned_state(config, session_backend) {
            Ok(backend) => backend,
            Err(error) => {
                warnings.push(format!("session backend unavailable: {error}"));
                None
            }
        };
    let sessions_cleared = match resolved_session_backend.as_ref() {
        Some(b) => match b.clear_agent_attribution(alias) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("session attribution clear: {e}"));
                0
            }
        },
        None => 0,
    };

    if !warnings.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"agent": alias, "warnings": warnings})),
            "owned-state cascade completed with warnings (some state may not have been removed)"
        );
    }

    let mut report = OwnedStateReport {
        memory_purged,
        knowledge_purged: knowledge_purge.total(),
        knowledge_foreign_edges_purged: knowledge_purge.affected_foreign_edges,
        cron_removed,
        acp_removed,
        sessions_cleared,
        archived_to: Some(archive_dir.display().to_string()),
        warnings,
    };

    // ── manifest: a self-describing record of the bundle ────────────────────
    let manifest = serde_json::json!({
        "alias": alias,
        "memory_rows": report.memory_purged,
        "knowledge_rows": report.knowledge_purged,
        "knowledge_foreign_edges": report.knowledge_foreign_edges_purged,
        "cron_jobs": report.cron_removed,
        "acp_sessions": report.acp_removed,
        "sessions_cleared": report.sessions_cleared,
        "warnings": report.warnings,
    });
    match serde_json::to_vec_pretty(&manifest).context("serialize cascade manifest") {
        Ok(bytes) => {
            if let Err(err) = write_json(&archive_dir.join("manifest.json"), bytes).await {
                report.warnings.push(archive_warning("manifest", &err));
            }
        }
        Err(err) => report.warnings.push(archive_warning("manifest", &err)),
    }

    report
}

/// What the agent-rename owned-state cascade re-pointed
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct RenameStateReport {
    pub memory_rows: usize,
    pub knowledge_rows: usize,
    pub cron_jobs: usize,
    pub acp_sessions: usize,
    pub sessions_repointed: usize,
    /// Surfaced failures. Non-empty means part of the cascade did NOT complete —
    /// those rows were not silently treated as re-pointed.
    pub warnings: Vec<String>,
}

pub async fn cascade_rename_agent(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    from: &str,
    to: &str,
) -> RenameStateReport {
    let mut warnings: Vec<String> = Vec::new();

    let resolved_memory = match resolve_memory_for_owned_state(config, mem) {
        Ok(memory) => memory,
        Err(error) => {
            warnings.push(format!("memory backend unavailable: {error}"));
            None
        }
    };

    let memory_rows = if let Some(mem) = resolved_memory.as_ref() {
        match mem.rename_agent(from, to).await {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("memory rename: {e}"));
                0
            }
        }
    } else {
        0
    };

    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_rows = match knowledge_path.try_exists() {
        Ok(true) => match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
            &knowledge_path,
            config.knowledge.max_nodes,
        ) {
            Ok(graph) => match graph.rename_owner(from, to) {
                Ok(n) => n,
                Err(e) => {
                    warnings.push(format!("knowledge rename: {e}"));
                    0
                }
            },
            Err(e) => {
                warnings.push(format!("knowledge graph open: {e}"));
                0
            }
        },
        Ok(false) => 0,
        Err(error) => {
            warnings.push(format!(
                "knowledge graph inspection ({}): {error}",
                knowledge_path.display()
            ));
            0
        }
    };

    let cron_jobs = match crate::cron::rename_jobs_by_agent(config, from, to) {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("cron rename: {e}"));
            0
        }
    };

    let acp_sessions = match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => match store.rename_sessions_by_agent(from, to) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("acp rename: {e}"));
                0
            }
        },
        Err(e) => {
            warnings.push(format!("acp store open: {e}"));
            0
        }
    };

    let resolved_session_backend =
        match resolve_session_backend_for_owned_state(config, session_backend) {
            Ok(backend) => backend,
            Err(error) => {
                warnings.push(format!("session backend unavailable: {error}"));
                None
            }
        };
    let sessions_repointed = match resolved_session_backend.as_ref() {
        Some(b) => match b.rename_agent_attribution(from, to) {
            Ok(n) => n,
            Err(e) => {
                warnings.push(format!("session attribution rename: {e}"));
                0
            }
        },
        None => 0,
    };

    if !warnings.is_empty() {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"from": from, "to": to, "warnings": warnings})),
            "rename owned-state cascade completed with warnings (some state may not have been re-pointed)"
        );
    }

    RenameStateReport {
        memory_rows,
        knowledge_rows,
        cron_jobs,
        acp_sessions,
        sessions_repointed,
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn archive_failure_preserves_cron_and_acp_for_retry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: tmp.path().join("data"),
            ..Default::default()
        };
        config.memory.backend = "none".to_string();
        config.gateway.session_persistence = false;
        config.channels.session_persistence = false;

        crate::cron::add_agent_job(
            &config,
            "agent_a",
            None,
            crate::cron::Schedule::Cron {
                expr: "*/5 * * * *".to_string(),
                tz: None,
            },
            "owned cron proof",
            crate::cron::SessionTarget::Isolated,
            None,
            None,
            false,
            None,
            false,
        )
        .unwrap();
        let acp = AcpSessionStore::new(&config.data_dir).unwrap();
        acp.create_session("owned-acp-proof", "agent_a", "/workspace")
            .unwrap();
        acp.mark_session_killed("owned-acp-proof").unwrap();
        drop(acp);

        let blocked_archive = tmp.path().join("blocked-archive");
        std::fs::write(&blocked_archive, "not a directory").unwrap();
        let first = cascade_owned_state(&config, None, None, "agent_a", &blocked_archive).await;
        assert_eq!(first.cron_removed, 0);
        assert_eq!(first.acp_removed, 0);
        assert!(
            first
                .warnings
                .iter()
                .any(|warning| warning.contains("cron archive"))
        );
        assert!(
            first
                .warnings
                .iter()
                .any(|warning| warning.contains("ACP archive"))
        );
        assert_eq!(
            crate::cron::list_jobs_by_agent(&config, "agent_a")
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            AcpSessionStore::new(&config.data_dir)
                .unwrap()
                .list_sessions_by_agent("agent_a")
                .unwrap()
                .len(),
            1
        );

        std::fs::remove_file(&blocked_archive).unwrap();
        std::fs::create_dir(&blocked_archive).unwrap();
        let retry = cascade_owned_state(&config, None, None, "agent_a", &blocked_archive).await;
        assert_eq!(retry.cron_removed, 1);
        assert_eq!(retry.acp_removed, 1);
        assert!(retry.warnings.is_empty(), "{:?}", retry.warnings);
        assert!(
            crate::cron::list_jobs_by_agent(&config, "agent_a")
                .unwrap()
                .is_empty()
        );
        assert!(
            AcpSessionStore::new(&config.data_dir)
                .unwrap()
                .list_sessions_by_agent("agent_a")
                .unwrap()
                .is_empty()
        );
    }
}
