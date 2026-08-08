//! Agent rename/delete **owned-state** cascades: the non-config half of
//! changing an agent alias lifecycle.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::AsyncWriteExt;
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

/// What the owned-state cascade removed.
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct OwnedStateReport {
    pub memory_purged: usize,
    pub knowledge_purged: usize,
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
    zeroclaw_runtime::i18n::get_required_cli_string_with_args(
        "cli-alias-knowledge-purge-skipped",
        &[("error", error.as_str())],
    )
}

pub async fn cascade_owned_state(
    config: &Config,
    mem: &Arc<dyn Memory>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
) -> OwnedStateReport {
    let cascade_dir = archive_dir.join("cascade");
    if let Err(err) = tokio::fs::create_dir_all(&cascade_dir).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": cascade_dir.display().to_string(), "err": err.to_string()})),
            "owned-state cascade: failed to create archive directory"
        );
    }
    let mut warnings: Vec<String> = Vec::new();

    // ── memory: export → archive → purge. Failures are SURFACED in `warnings`,
    // not masked as 0 (markdown/none have no DB rows — their memory lives in the
    // archived workspace — but a real backend error must stay visible). ────────
    let mem_rows = match mem.export_agent(alias).await {
        Ok(rows) => rows,
        Err(e) => {
            warnings.push(format!("memory export: {e}"));
            Vec::new()
        }
    };
    match serde_json::to_vec_pretty(&mem_rows).context("serialize memory export") {
        Ok(bytes) => {
            if let Err(err) = write_json(&cascade_dir.join("memory.json"), bytes).await {
                warnings.push(archive_warning("memory", &err));
            }
        }
        Err(err) => warnings.push(archive_warning("memory", &err)),
    }
    let memory_purged = match mem.purge_agent(alias).await {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("memory purge: {e}"));
            0
        }
    };

    // ── knowledge graph: export → archive → purge. The graph owns durable
    // alias attribution, so it participates in the same canonical lifecycle
    // cascade as memory/session state. Avoid creating an empty DB when the
    // operator has never enabled knowledge.
    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_purged = if knowledge_path.exists() {
        match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
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
                    Ok(()) => match graph.purge_owner(alias) {
                        Ok(n) => n,
                        Err(e) => {
                            warnings.push(format!("knowledge purge: {e}"));
                            0
                        }
                    },
                    Err(err) => {
                        warnings.push(knowledge_purge_skipped_warning(&err));
                        0
                    }
                },
                Err(err) => {
                    warnings.push(knowledge_purge_skipped_warning(&err));
                    0
                }
            },
            Err(e) => {
                warnings.push(format!("knowledge graph open: {e}"));
                0
            }
        }
    } else {
        0
    };

    // ── cron: list → archive → remove (cron_runs cascade off job_id) ─────────
    let cron_jobs = match zeroclaw_runtime::cron::list_jobs_by_agent(config, alias) {
        Ok(jobs) => jobs,
        Err(e) => {
            warnings.push(format!("cron list: {e}"));
            Vec::new()
        }
    };
    match serde_json::to_vec_pretty(&cron_jobs).context("serialize cron export") {
        Ok(bytes) => {
            if let Err(err) = write_json(&cascade_dir.join("cron.json"), bytes).await {
                warnings.push(archive_warning("cron", &err));
            }
        }
        Err(err) => warnings.push(archive_warning("cron", &err)),
    }
    let cron_removed = match zeroclaw_runtime::cron::remove_jobs_by_agent(config, alias) {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("cron remove: {e}"));
            0
        }
    };

    // ── acp: list → archive → delete (only killed sessions remain) ───────────
    let mut acp_removed = 0;
    match AcpSessionStore::new(&config.data_dir) {
        Ok(store) => {
            let sessions = store.list_sessions_by_agent(alias).unwrap_or_default();
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
                Ok(bytes) => {
                    if let Err(err) = write_json(&cascade_dir.join("acp.json"), bytes).await {
                        warnings.push(archive_warning("ACP", &err));
                    }
                }
                Err(err) => warnings.push(archive_warning("ACP", &err)),
            }
            match store.delete_sessions_by_agent(alias) {
                Ok(n) => acp_removed = n,
                Err(e) => warnings.push(format!("acp delete: {e}")),
            }
        }
        Err(e) => warnings.push(format!("acp store open: {e}")),
    }

    // ── session metadata: clear the stale agent attribution (keep the convo) ─
    let sessions_cleared = match session_backend {
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
        knowledge_purged,
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
#[derive(Debug, Default, Clone, serde::Serialize)]
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
    mem: &Arc<dyn Memory>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    from: &str,
    to: &str,
) -> RenameStateReport {
    let mut warnings: Vec<String> = Vec::new();

    let memory_rows = match mem.rename_agent(from, to).await {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("memory rename: {e}"));
            0
        }
    };

    let knowledge_path = config.knowledge.resolved_db_path();
    let knowledge_rows = if knowledge_path.exists() {
        match zeroclaw_memory::knowledge_graph::KnowledgeGraph::new(
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
        }
    } else {
        0
    };

    let cron_jobs = match zeroclaw_runtime::cron::rename_jobs_by_agent(config, from, to) {
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

    let sessions_repointed = match session_backend {
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
