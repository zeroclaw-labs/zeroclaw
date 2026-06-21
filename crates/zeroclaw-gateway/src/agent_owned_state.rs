//! Agent-deletion **owned-state** cascade (#7175) — the non-config half of
//! deleting an agent.
//!
//! Config references are scrubbed by
//! `zeroclaw_config::alias_refs::delete_with_cascade`. This module handles the
//! persisted state the *surface* owns (the infra stores `zeroclaw-config` can't
//! depend on): memory rows, cron jobs, ACP sessions, and session-metadata
//! attribution. Each store is opened from `config.data_dir` on demand.
//!
//! **Export-then-delete** (recoverable, per the agreed cascade policy): rows are
//! written into the same `agents/_deleted/<alias>-<ts>/` archive as the
//! workspace (under `cascade/`), then removed. Best-effort + reported — a single
//! store failing does not abort the others (mirrors the existing
//! archive+purge behaviour); the surface persists the config last.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

/// Count of *live* (un-killed) ACP sessions owned by `alias`. Non-zero is a HARD
/// blocker for deleting the agent. Returns `Err` if the ACP store exists but
/// can't be opened/queried — the caller must **fail closed** (refuse the delete)
/// rather than assume zero, since live sessions might exist undetected.
/// (`AcpSessionStore::new` creates a missing DB empty, so a fresh system with no
/// ACP usage returns `Ok(0)` and deletes proceed normally.)
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
    pub cron_removed: usize,
    pub acp_removed: usize,
    pub sessions_cleared: usize,
    pub archived_to: Option<String>,
    /// Surfaced failures (export / purge / delete errors). Non-empty means part
    /// of the cascade did NOT complete — those rows were not silently treated as
    /// removed. The handler logs these; nothing is masked as success.
    pub warnings: Vec<String>,
}

async fn write_json(path: &Path, bytes: Vec<u8>) {
    if let Err(err) = tokio::fs::write(path, bytes).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"path": path.display().to_string(), "err": err.to_string()})),
            "owned-state cascade: failed to write archive file"
        );
    }
}

/// Export-then-delete the agent's owned non-config state into `archive_dir`
/// (the `agents/_deleted/<alias>-<ts>/` the workspace was moved to), then return
/// a report. Best-effort: each store is independent.
///
/// Precondition: the caller already refused if [`live_acp_session_count`] was
/// non-zero, so only killed ACP sessions remain to delete here.
pub async fn cascade_owned_state(
    config: &Config,
    mem: &Arc<dyn Memory>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
) -> OwnedStateReport {
    let cascade_dir = archive_dir.join("cascade");
    let _ = tokio::fs::create_dir_all(&cascade_dir).await;
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
    if let Ok(bytes) = serde_json::to_vec_pretty(&mem_rows) {
        write_json(&cascade_dir.join("memory.json"), bytes).await;
    }
    let memory_purged = match mem.purge_agent(alias).await {
        Ok(n) => n,
        Err(e) => {
            warnings.push(format!("memory purge: {e}"));
            0
        }
    };

    // ── cron: list → archive → remove (cron_runs cascade off job_id) ─────────
    let cron_jobs = match zeroclaw_runtime::cron::list_jobs_by_agent(config, alias) {
        Ok(jobs) => jobs,
        Err(e) => {
            warnings.push(format!("cron list: {e}"));
            Vec::new()
        }
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&cron_jobs) {
        write_json(&cascade_dir.join("cron.json"), bytes).await;
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
            if let Ok(bytes) = serde_json::to_vec_pretty(&json) {
                write_json(&cascade_dir.join("acp.json"), bytes).await;
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

    let report = OwnedStateReport {
        memory_purged,
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
        "cron_jobs": report.cron_removed,
        "acp_sessions": report.acp_removed,
        "sessions_cleared": report.sessions_cleared,
        "warnings": report.warnings,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&manifest) {
        write_json(&archive_dir.join("manifest.json"), bytes).await;
    }

    report
}

/// What the agent-rename owned-state cascade re-pointed (#7468).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RenameStateReport {
    pub memory_rows: usize,
    pub cron_jobs: usize,
    pub acp_sessions: usize,
    pub sessions_repointed: usize,
    /// Surfaced failures. Non-empty means part of the cascade did NOT complete —
    /// those rows were not silently treated as re-pointed.
    pub warnings: Vec<String>,
}

/// Re-point the agent's owned non-config **DB** state from `from` to `to`:
/// memory rows (`agents.alias`), cron jobs, ACP sessions, and session-metadata
/// attribution. The workspace directory is moved by the caller (mirroring how
/// the delete handler archives the workspace, not `cascade_owned_state`).
///
/// Unlike delete this is **in-place** — no export/archive — and there is **no
/// live-session refusal**: a live ACP session simply follows the rename.
/// Best-effort + reported: a single store failing does not abort the others; the
/// surface persists the renamed config last.
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
        cron_jobs,
        acp_sessions,
        sessions_repointed,
        warnings,
    }
}
