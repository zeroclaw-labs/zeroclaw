//! Shared agent lifecycle primitives used by RPC, gateway, and CLI adapters.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::alias_refs::{self, AliasKind};
use zeroclaw_config::schema::Config;
use zeroclaw_infra::acp_session_store::AcpSessionStore;
use zeroclaw_infra::session_backend::SessionBackend;

#[derive(Debug, Clone)]
pub struct AgentDeletePreflight {
    pub alias: String,
    pub allowed: bool,
    pub blockers: Vec<String>,
    pub scrubs: Vec<String>,
    pub owned_state: Vec<String>,
    pub workspace: Option<PathBuf>,
}

/// Reusable, fail-closed agent deletion preflight.
#[must_use]
pub fn plan_agent_delete(config: &Config, alias: &str) -> AgentDeletePreflight {
    let live_acp = live_acp_session_count(config, alias).map_err(|error| error.to_string());
    plan_agent_delete_with_acp_count(config, alias, live_acp)
}

/// Build a deletion preflight from an ACP count obtained by the owning adapter.
/// RPC uses this form so SQLite work runs off the Tokio worker and reuses the
/// daemon-owned store.
#[must_use]
pub fn plan_agent_delete_with_acp_count(
    config: &Config,
    alias: &str,
    live_acp: Result<usize, String>,
) -> AgentDeletePreflight {
    if alias_refs::is_reserved_agent_alias(alias) {
        return AgentDeletePreflight {
            alias: alias.to_string(),
            allowed: false,
            blockers: vec!["the `default` agent is reserved and cannot be deleted".to_string()],
            scrubs: Vec::new(),
            owned_state: Vec::new(),
            workspace: None,
        };
    }
    if !config.agents.contains_key(alias) {
        return AgentDeletePreflight {
            alias: alias.to_string(),
            allowed: false,
            blockers: vec![format!("agents.{alias} is not configured")],
            scrubs: Vec::new(),
            owned_state: Vec::new(),
            workspace: None,
        };
    }

    let plan = alias_refs::plan_delete(config, &AliasKind::Agent, alias);
    let mut blockers: Vec<String> = plan
        .blockers
        .iter()
        .map(|site| format!("{} (hard config reference)", site.path))
        .collect();
    let live_acp = match live_acp {
        Ok(0) => Some(0),
        Ok(count) => {
            blockers.push(format!("{count} live ACP session(s) - end them first"));
            Some(count)
        }
        Err(error) => {
            blockers.push(format!(
                "could not verify live ACP sessions ({error}); refusing to avoid orphaning active sessions"
            ));
            None
        }
    };
    let mut owned_state = vec![
        "memory records (if any)".to_string(),
        "cron jobs (if any)".to_string(),
        "control-plane tasks (if any)".to_string(),
        "session attribution (if any)".to_string(),
    ];
    owned_state.push(match live_acp {
        Some(count) => format!("ACP sessions (if any; {count} live)"),
        None => "ACP sessions (state unavailable)".to_string(),
    });
    let workspace = config.agent_workspace_dir(alias);
    if workspace.exists() {
        owned_state.insert(0, format!("workspace ({})", workspace.display()));
    }

    AgentDeletePreflight {
        alias: alias.to_string(),
        allowed: blockers.is_empty(),
        blockers,
        scrubs: plan.scrubs.into_iter().map(|site| site.path).collect(),
        owned_state,
        workspace: Some(workspace),
    }
}

pub fn live_acp_session_count(config: &Config, alias: &str) -> anyhow::Result<usize> {
    let store = AcpSessionStore::new(&config.data_dir)
        .context("open ACP session store to verify live sessions")?;
    store
        .count_live_sessions_by_agent(alias)
        .context("count live ACP sessions for agent")
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct WorkspaceArchiveReport {
    pub archive_dir: PathBuf,
    pub warnings: Vec<String>,
}

pub async fn archive_agent_workspace(
    config: &Config,
    alias: &str,
    workspace: &Path,
) -> WorkspaceArchiveReport {
    let archive_root = config.data_dir.join("agents").join("_deleted");
    let ts = chrono::Utc::now().format("%Y%m%d%H%M%S");
    let mut warnings = Vec::new();
    if let Err(error) = tokio::fs::create_dir_all(&archive_root).await {
        ::zeroclaw_log::record!(
            WARN,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                .with_attrs(::serde_json::json!({"agent": alias, "archive": archive_root.display().to_string(), "err": error.to_string()})),
            "agent delete: archive dir creation failed"
        );
        warnings.push(format!(
            "archive dir creation failed ({}): {error}",
            archive_root.display()
        ));
    }
    let archive_dir = archive_root.join(format!("{alias}-{ts}-{}", uuid::Uuid::new_v4()));
    if let Err(error) = tokio::fs::create_dir(&archive_dir).await {
        warnings.push(format!(
            "archive dir allocation failed ({}): {error}",
            archive_dir.display()
        ));
    }
    if workspace.exists() {
        let destination = archive_dir.join("workspace");
        if let Err(error) = tokio::fs::rename(workspace, &destination).await {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({"agent": alias, "from": workspace.display().to_string(), "to": destination.display().to_string(), "err": error.to_string()})),
                "agent delete: workspace archive failed"
            );
            warnings.push(format!(
                "workspace archive failed ({} -> {}): {error}",
                workspace.display(),
                destination.display()
            ));
        }
    }
    WorkspaceArchiveReport {
        archive_dir,
        warnings,
    }
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct OwnedStateReport {
    pub memory_purged: usize,
    pub cron_removed: usize,
    pub acp_removed: usize,
    pub sessions_cleared: usize,
    pub control_plane_tasks_removed: usize,
    pub archived_to: Option<String>,
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

pub async fn cascade_owned_state(
    config: &Config,
    mem: &Arc<dyn Memory>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    alias: &str,
    archive_dir: &Path,
) -> OwnedStateReport {
    let cascade_dir = archive_dir.join("cascade");
    let _ = tokio::fs::create_dir_all(&cascade_dir).await;
    let mut warnings = Vec::new();

    let mem_rows = match mem.export_agent(alias).await {
        Ok(rows) => rows,
        Err(error) => {
            warnings.push(format!("memory export: {error}"));
            Vec::new()
        }
    };
    if let Ok(bytes) = serde_json::to_vec_pretty(&mem_rows) {
        write_json(&cascade_dir.join("memory.json"), bytes).await;
    }
    let memory_purged = match mem.purge_agent(alias).await {
        Ok(count) => count,
        Err(error) => {
            warnings.push(format!("memory purge: {error}"));
            0
        }
    };

    let cron_config = config.clone();
    let cron_alias = alias.to_string();
    let cron_jobs_result = match tokio::task::spawn_blocking(move || {
        crate::cron::list_jobs_by_agent(&cron_config, &cron_alias)
    })
    .await
    {
        Ok(result) => result,
        Err(error) => {
            warnings.push(format!("cron list task failed: {error}"));
            Ok(Vec::new())
        }
    };
    let cron_jobs = cron_jobs_result.unwrap_or_else(|error| {
        warnings.push(format!("cron list: {error}"));
        Vec::new()
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&cron_jobs) {
        write_json(&cascade_dir.join("cron.json"), bytes).await;
    }
    let cron_config = config.clone();
    let cron_alias = alias.to_string();
    let cron_removed = match tokio::task::spawn_blocking(move || {
        crate::cron::remove_jobs_by_agent(&cron_config, &cron_alias)
    })
    .await
    {
        Ok(Ok(count)) => count,
        Ok(Err(error)) => {
            warnings.push(format!("cron remove: {error}"));
            0
        }
        Err(error) => {
            warnings.push(format!("cron remove task failed: {error}"));
            0
        }
    };

    let acp_data_dir = config.data_dir.clone();
    let acp_alias = alias.to_string();
    let acp_export = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        let store = AcpSessionStore::new(&acp_data_dir)?;
        let sessions = store.list_sessions_by_agent(&acp_alias).unwrap_or_default();
        let json = sessions
            .iter()
            .map(|session| {
                serde_json::json!({
                    "session_uuid": session.session_uuid,
                    "agent_alias": session.agent_alias,
                    "workspace_dir": session.workspace_dir,
                    "token_count": session.token_count,
                    "message_count": session.message_count,
                    "created_at": session.created_at.to_rfc3339(),
                    "last_activity": session.last_activity.to_rfc3339(),
                })
            })
            .collect::<Vec<_>>();
        Ok(json)
    })
    .await;
    match acp_export {
        Ok(Ok(json)) => {
            if let Ok(bytes) = serde_json::to_vec_pretty(&json) {
                write_json(&cascade_dir.join("acp.json"), bytes).await;
            }
        }
        Ok(Err(error)) => {
            warnings.push(format!("acp export: {error}"));
        }
        Err(error) => {
            warnings.push(format!("acp export task failed: {error}"));
        }
    }
    let acp_data_dir = config.data_dir.clone();
    let acp_alias = alias.to_string();
    let acp_removed = match tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
        AcpSessionStore::new(&acp_data_dir)?.delete_sessions_by_agent(&acp_alias)
    })
    .await
    {
        Ok(Ok(count)) => count,
        Ok(Err(error)) => {
            warnings.push(format!("acp delete: {error}"));
            0
        }
        Err(error) => {
            warnings.push(format!("acp delete task failed: {error}"));
            0
        }
    };

    let sessions_cleared = match session_backend {
        Some(backend) => {
            let backend = Arc::clone(backend);
            let alias = alias.to_string();
            match tokio::task::spawn_blocking(move || backend.clear_agent_attribution(&alias)).await
            {
                Ok(Ok(count)) => count,
                Ok(Err(error)) => {
                    warnings.push(format!("session attribution clear: {error}"));
                    0
                }
                Err(error) => {
                    warnings.push(format!("session attribution task failed: {error}"));
                    0
                }
            }
        }
        None => 0,
    };

    let control_plane_tasks_removed = if config.data_dir.join("control_plane.db").exists() {
        let data_dir = config.data_dir.clone();
        let alias = alias.to_string();
        match tokio::task::spawn_blocking(move || {
            crate::control_plane::SqliteTaskStore::new(&data_dir)?
                .delete_by_agent(&alias)
                .map(|count| count as usize)
        })
        .await
        {
            Ok(Ok(count)) => count,
            Ok(Err(error)) => {
                warnings.push(format!("control-plane task delete: {error}"));
                0
            }
            Err(error) => {
                warnings.push(format!("control-plane task delete task failed: {error}"));
                0
            }
        }
    } else {
        0
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
        control_plane_tasks_removed,
        archived_to: Some(archive_dir.display().to_string()),
        warnings,
    };
    let manifest = serde_json::json!({
        "alias": alias,
        "memory_rows": report.memory_purged,
        "cron_jobs": report.cron_removed,
        "acp_sessions": report.acp_removed,
        "sessions_cleared": report.sessions_cleared,
        "control_plane_tasks": report.control_plane_tasks_removed,
        "warnings": report.warnings,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&manifest) {
        write_json(&archive_dir.join("manifest.json"), bytes).await;
    }
    report
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct RenameStateReport {
    pub memory_rows: usize,
    pub cron_jobs: usize,
    pub acp_sessions: usize,
    pub sessions_repointed: usize,
    pub warnings: Vec<String>,
}

pub async fn cascade_rename_agent(
    config: &Config,
    mem: Option<&Arc<dyn Memory>>,
    session_backend: Option<&Arc<dyn SessionBackend>>,
    from: &str,
    to: &str,
) -> RenameStateReport {
    let mut warnings = Vec::new();
    let memory_rows = match mem {
        Some(mem) => mem.rename_agent(from, to).await.unwrap_or_else(|error| {
            warnings.push(format!("memory rename: {error}"));
            0
        }),
        None => 0,
    };
    let blocking_config = config.clone();
    let blocking_backend = session_backend.cloned();
    let blocking_from = from.to_string();
    let blocking_to = to.to_string();
    let blocking = tokio::task::spawn_blocking(move || {
        let mut warnings = Vec::new();
        let cron_jobs =
            crate::cron::rename_jobs_by_agent(&blocking_config, &blocking_from, &blocking_to)
                .unwrap_or_else(|error| {
                    warnings.push(format!("cron rename: {error}"));
                    0
                });
        let acp_sessions = match AcpSessionStore::new(&blocking_config.data_dir) {
            Ok(store) => store
                .rename_sessions_by_agent(&blocking_from, &blocking_to)
                .unwrap_or_else(|error| {
                    warnings.push(format!("acp rename: {error}"));
                    0
                }),
            Err(error) => {
                warnings.push(format!("acp store open: {error}"));
                0
            }
        };
        let sessions_repointed = match blocking_backend {
            Some(backend) => backend
                .rename_agent_attribution(&blocking_from, &blocking_to)
                .unwrap_or_else(|error| {
                    warnings.push(format!("session attribution rename: {error}"));
                    0
                }),
            None => 0,
        };
        (cron_jobs, acp_sessions, sessions_repointed, warnings)
    })
    .await;
    let (cron_jobs, acp_sessions, sessions_repointed, blocking_warnings) = match blocking {
        Ok(result) => result,
        Err(error) => (0, 0, 0, vec![format!("rename state task failed: {error}")]),
    };
    warnings.extend(blocking_warnings);
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

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config};

    #[test]
    fn preflight_refuses_reserved_and_missing_aliases() {
        let config = Config::default();
        let reserved = plan_agent_delete(&config, "default");
        assert!(!reserved.allowed);
        assert!(reserved.blockers[0].contains("reserved"));

        let missing = plan_agent_delete(&config, "missing");
        assert!(!missing.allowed);
        assert!(missing.blockers[0].contains("not configured"));
    }

    #[test]
    fn preflight_surfaces_scrubs_for_configured_agent() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        config
            .agents
            .insert("victim".to_string(), AliasedAgentConfig::default());
        config.acp.default_agent = Some("victim".to_string());

        let preview = plan_agent_delete(&config, "victim");
        assert!(preview.allowed, "{:?}", preview.blockers);
        assert!(
            preview
                .scrubs
                .iter()
                .any(|path| path == "acp.default_agent")
        );
    }

    #[tokio::test]
    async fn archive_paths_are_unique_for_rapid_repeated_deletes() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        let missing = temp.path().join("missing-workspace");

        let first = archive_agent_workspace(&config, "rapid", &missing).await;
        let second = archive_agent_workspace(&config, "rapid", &missing).await;

        assert_ne!(first.archive_dir, second.archive_dir);
        assert!(first.archive_dir.is_dir());
        assert!(second.archive_dir.is_dir());
    }

    #[test]
    fn preflight_refuses_hard_config_reference_and_live_acp_session() {
        let temp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        config
            .agents
            .insert("victim".to_string(), AliasedAgentConfig::default());
        config.heartbeat.enabled = true;
        config.heartbeat.agent = "victim".to_string();
        AcpSessionStore::new(&config.data_dir)
            .unwrap()
            .create_session("live", "victim", "/tmp/victim")
            .unwrap();

        let preview = plan_agent_delete(&config, "victim");
        assert!(!preview.allowed);
        assert!(
            preview
                .blockers
                .iter()
                .any(|blocker| blocker.contains("heartbeat.agent"))
        );
        assert!(
            preview
                .blockers
                .iter()
                .any(|blocker| blocker.contains("1 live ACP session"))
        );
    }

    #[test]
    fn preflight_fails_closed_when_acp_state_cannot_be_opened() {
        let temp = tempfile::TempDir::new().unwrap();
        let data_file = temp.path().join("data-file");
        std::fs::write(&data_file, "not a directory").unwrap();
        let mut config = Config {
            data_dir: data_file,
            ..Config::default()
        };
        config
            .agents
            .insert("victim".to_string(), AliasedAgentConfig::default());

        let preview = plan_agent_delete(&config, "victim");
        assert!(!preview.allowed);
        assert!(
            preview
                .blockers
                .iter()
                .any(|blocker| blocker.contains("could not verify live ACP sessions"))
        );
    }

    #[tokio::test]
    async fn archive_agent_workspace_moves_existing_workspace() {
        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("AGENTS.md"), "test").unwrap();

        let report = archive_agent_workspace(&config, "victim", &workspace).await;
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(!workspace.exists());
        assert!(report.archive_dir.join("workspace/AGENTS.md").exists());
    }

    #[tokio::test]
    async fn owned_state_cascade_removes_control_plane_tasks() {
        use crate::control_plane::{
            SqliteTaskStore, TaskKind, TaskRecord, TaskRegistry, TaskStatus,
        };

        let temp = tempfile::TempDir::new().unwrap();
        let config = Config {
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        let store = SqliteTaskStore::new(&config.data_dir).unwrap();
        store
            .create(TaskRecord {
                id: "delete-task".into(),
                kind: TaskKind::Delegate,
                agent: "victim".into(),
                status: TaskStatus::Completed,
                owner_pid: 0,
                owner_boot_id: String::new(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: None,
                delivered: true,
                idem_key: None,
                principal_id: None,
                started_at: "2026-08-26T00:00:00Z".into(),
                finished_at: Some("2026-08-26T00:00:01Z".into()),
            })
            .await
            .unwrap();
        let memory: Arc<dyn Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("none"));
        let archive_dir = temp.path().join("archive");

        let report = cascade_owned_state(&config, &memory, None, "victim", &archive_dir).await;

        assert_eq!(report.control_plane_tasks_removed, 1);
        assert_eq!(store.count_by_agent("victim").unwrap(), 0);
    }
}
