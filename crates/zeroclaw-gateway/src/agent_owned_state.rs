//! Agent-deletion **owned-state** cascade— the non-config half of
//! deleting an agent.

use std::path::Path;
use std::sync::Arc;

use anyhow::Context;
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
    pub cron_removed: usize,
    pub acp_removed: usize,
    pub sessions_cleared: usize,
    pub archived_to: Option<String>,
    /// Surfaced failures (export / purge / delete errors). Non-empty means part
    /// of the cascade did NOT complete — those rows were not silently treated as
    /// removed. The handler logs these; nothing is masked as success.
    pub warnings: Vec<String>,
}

async fn write_json(path: &Path, value: serde_json::Value) -> anyhow::Result<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || {
        zeroclaw_infra::owned_archive::write_verified_json(&path, &value)
    })
    .await?
}

fn removed_or_retained(
    store: &str,
    result: anyhow::Result<usize>,
    warnings: &mut Vec<String>,
) -> usize {
    match result {
        Ok(count) => count,
        Err(error) => {
            warnings.push(format!(
                "{store} archive/purge: {error:#}; original records retained"
            ));
            0
        }
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
    let mut warnings: Vec<String> = Vec::new();

    // Each store owns the guarded export → verified archive → exact deletion.
    // An unsupported backend or any failure retains that store's original data.
    // There is intentionally no cross-store transaction or unguarded fallback.
    let memory_purged = removed_or_retained(
        "memory",
        mem.archive_and_purge_agent(alias, &cascade_dir.join("memory.json"))
            .await,
        &mut warnings,
    );

    let owned_config = config.clone();
    let owned_alias = alias.to_owned();
    let stores = tokio::task::spawn_blocking(move || {
        let cron = zeroclaw_runtime::cron::archive_and_remove_jobs_by_agent(
            &owned_config,
            &owned_alias,
            &cascade_dir.join("cron.json"),
        );
        let acp = AcpSessionStore::new(&owned_config.data_dir).and_then(|store| {
            store.archive_and_delete_sessions_by_agent(&owned_alias, &cascade_dir.join("acp.json"))
        });
        (cron, acp)
    })
    .await;
    let (cron_removed, acp_removed) = match stores {
        Ok((cron, acp)) => (
            removed_or_retained("cron", cron, &mut warnings),
            removed_or_retained("acp", acp, &mut warnings),
        ),
        Err(error) => {
            warnings.push(format!(
                "owned-state archive worker failed: {error}; inspect each archive before retrying"
            ));
            (0, 0)
        }
    };

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
    if let Err(error) = write_json(&archive_dir.join("manifest.json"), manifest).await {
        report.warnings.push(format!("archive manifest: {error:#}"));
    }

    report
}

/// What the agent-rename owned-state cascade re-pointed
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

    // Identity is already bound by the config API/CLI before committing the
    // rename. This best-effort follower must not overwrite that authority
    // after awaits or on a retry of an older owned-state cascade.

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

#[cfg(test)]
mod archive_tests {
    use super::*;
    use zeroclaw_api::memory_traits::MemoryCategory;
    use zeroclaw_config::schema::MemoryConfig;
    use zeroclaw_memory::{TACHI_COMPILED, create_memory};

    #[tokio::test]
    async fn failed_archive_keeps_memory_and_reports_independent_store_results() {
        if !TACHI_COMPILED {
            return;
        }
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: directory.path().join("data"),
            ..Config::default()
        };
        let mem_cfg = MemoryConfig {
            backend: "tachi".into(),
            hygiene_enabled: false,
            ..MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(create_memory(&mem_cfg, &config.data_dir, None).unwrap());
        let agent_id = memory.ensure_agent_uuid("alpha").await.unwrap();
        memory
            .store_with_agent(
                "note",
                "must survive",
                MemoryCategory::Core,
                None,
                None,
                None,
                Some(&agent_id),
            )
            .await
            .unwrap();
        let archive = directory.path().join("archive");
        std::fs::create_dir_all(archive.join("cascade")).unwrap();
        std::fs::create_dir(archive.join("cascade/memory.json")).unwrap();
        let report = cascade_owned_state(&config, &memory, None, "alpha", &archive).await;
        assert_eq!(report.memory_purged, 0);
        assert_eq!(
            memory.export_agent("alpha").await.unwrap()[0].content,
            "must survive"
        );
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("memory archive/purge"))
        );
        assert!(archive.join("cascade/cron.json").is_file());
        assert!(archive.join("cascade/acp.json").is_file());
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(archive.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["memory_rows"], 0);
        assert!(!manifest["warnings"].as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn unsupported_memory_backend_reports_retention_without_purge_fallback() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            data_dir: directory.path().join("data"),
            ..Config::default()
        };
        let memory: Arc<dyn Memory> = Arc::new(zeroclaw_memory::none::NoneMemory::new("none"));
        let report = cascade_owned_state(
            &config,
            &memory,
            None,
            "alpha",
            &directory.path().join("archive"),
        )
        .await;
        assert_eq!(report.memory_purged, 0);
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("unsupported") && warning.contains("retained"))
        );
    }
}
