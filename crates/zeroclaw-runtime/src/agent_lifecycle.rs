//! Shared agent lifecycle primitives used by RPC, gateway, and CLI adapters.

use std::path::PathBuf;
#[cfg(test)]
use std::sync::Arc;

#[cfg(test)]
use zeroclaw_api::memory_traits::Memory;
use zeroclaw_config::alias_refs::{self, AliasKind};
use zeroclaw_config::schema::Config;

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
    plan_agent_delete_inner(config, alias, live_acp, false)
}

/// Check a committed-delete retry without treating the absent config entry as
/// permission to bypass hard references or the live ACP gate.
#[must_use]
pub fn plan_agent_delete_recovery_with_acp_count(
    config: &Config,
    alias: &str,
    live_acp: Result<usize, String>,
) -> AgentDeletePreflight {
    plan_agent_delete_inner(config, alias, live_acp, true)
}

fn plan_agent_delete_inner(
    config: &Config,
    alias: &str,
    live_acp: Result<usize, String>,
    recovery: bool,
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
    if !recovery && !config.agents.contains_key(alias) {
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

// Cleanup is canonical in agent_owned_state. This module owns lifecycle
// preflight only; compatibility exports do not allocate or persist state.
pub use crate::agent_owned_state::{
    archive_agent_workspace, cascade_owned_state, cascade_rename_agent, live_acp_session_count,
};

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config};
    use zeroclaw_infra::acp_session_store::AcpSessionStore;

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

        assert_ne!(first.path, second.path);
        assert!(first.path.is_dir());
        assert!(second.path.is_dir());
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
            .create_session("live", "victim", "/tmp/victim", None)
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
        assert!(report.path.join("workspace/AGENTS.md").exists());
    }

    #[tokio::test]
    async fn owned_state_cascade_removes_control_plane_tasks() {
        use crate::control_plane::{
            SqliteTaskStore, TaskKind, TaskRecord, TaskRegistry, TaskStatus,
        };

        let temp = tempfile::TempDir::new().unwrap();
        let mut config = Config {
            config_path: temp.path().join("config.toml"),
            data_dir: temp.path().join("data"),
            ..Config::default()
        };
        config.memory.backend = "none".into();
        config.knowledge.db_path = temp
            .path()
            .join("knowledge.db")
            .to_string_lossy()
            .into_owned();
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
                originator_chain: Vec::new(),
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

        assert_eq!(
            SqliteTaskStore::count_existing_by_agent(&config.data_dir, "victim").unwrap(),
            1
        );
        assert!(
            crate::agent_owned_state::committed_delete_residue_exists(
                &config,
                Some(&memory),
                None,
                "victim"
            )
            .await
        );
        let report =
            cascade_owned_state(&config, Some(&memory), None, "victim", &archive_dir).await;

        assert_eq!(report.control_plane_tasks_removed, 1);
        assert_eq!(store.count_by_agent("victim").unwrap(), 0);
        assert!(
            !crate::agent_owned_state::committed_delete_residue_exists(
                &config,
                Some(&memory),
                None,
                "victim"
            )
            .await
        );
    }
}
