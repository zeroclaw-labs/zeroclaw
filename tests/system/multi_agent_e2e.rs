//! End-to-end tests for the multi-agent runtime.
//!
//! Covers install-level upgrade and per-agent lifecycle paths that
//! cross multiple subsystems (config schema, filesystem migration,
//! per-agent memory, agents-table machinery). Tests run against a
//! TempDir-rooted install so they're hermetic and can be run in
//! parallel.

use tempfile::TempDir;

/// Filesystem migration: a legacy `<install>/workspace/` is split on
/// first boot — shared databases (`memory/`, `sessions/`, `state/`)
/// move to `<install>/data/`, per-agent plaintext (MEMORY.md,
/// IDENTITY.md, SOUL.md, anything else) moves to
/// `<install>/agents/default/workspace/`. Timestamped backup retains
/// the legacy tree; re-run on a fresh-cleaned install is a no-op.
#[test]
fn legacy_install_upgrades_cleanly_with_backup() {
    let tmp = TempDir::new().unwrap();
    let install_root = tmp.path();

    // Seed the legacy single-workspace layout.
    let legacy = install_root.join("workspace");
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(
        legacy.join("MEMORY.md"),
        "# Long-Term Memory\n\nlegacy data",
    )
    .unwrap();
    std::fs::write(legacy.join("AGENTS.md"), "legacy identity").unwrap();
    // Shared-database subdir: this should land under <install>/data/,
    // not under the per-agent workspace.
    let legacy_db = legacy.join("memory");
    std::fs::create_dir_all(&legacy_db).unwrap();
    std::fs::write(legacy_db.join("brain.db"), b"sqlite-bytes").unwrap();

    let ran = zeroclaw_config::migration::migrate_legacy_workspace_to_default_agent(install_root)
        .expect("migration must succeed on populated legacy install");
    assert!(ran, "populated legacy install → split migration runs");

    // Legacy dir is gone; both target dirs are populated with the right
    // pieces of the legacy tree.
    assert!(!legacy.exists(), "legacy workspace must move out");
    let new_default = install_root
        .join("agents")
        .join("default")
        .join("workspace");
    assert_eq!(
        std::fs::read_to_string(new_default.join("MEMORY.md")).unwrap(),
        "# Long-Term Memory\n\nlegacy data",
        "MEMORY.md must land in the per-agent workspace"
    );
    assert_eq!(
        std::fs::read_to_string(new_default.join("AGENTS.md")).unwrap(),
        "legacy identity",
        "AGENTS.md must land in the per-agent workspace"
    );

    let data_target = install_root.join("data");
    assert_eq!(
        std::fs::read(data_target.join("memory").join("brain.db")).unwrap(),
        b"sqlite-bytes",
        "shared databases must land under <install>/data/"
    );
    assert!(
        !new_default.join("memory").exists(),
        "shared-db subdir must NOT land in the per-agent workspace"
    );

    // A timestamped backup retains the legacy contents — operator
    // can roll back by moving the backup back into place.
    let backups: Vec<_> = std::fs::read_dir(install_root)
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| {
            e.file_name()
                .to_str()
                .is_some_and(|s| s.starts_with("backup-"))
        })
        .collect();
    assert_eq!(backups.len(), 1, "exactly one backup dir");
    let backup_legacy = backups[0].path().join("legacy-workspace");
    assert_eq!(
        std::fs::read_to_string(backup_legacy.join("MEMORY.md")).unwrap(),
        "# Long-Term Memory\n\nlegacy data",
        "backup must retain pre-migration contents"
    );
    assert_eq!(
        std::fs::read(backup_legacy.join("memory").join("brain.db")).unwrap(),
        b"sqlite-bytes",
        "backup must retain the shared-db subdir too"
    );

    // Idempotent re-run: legacy gone → no-op (returns false).
    let ran_again =
        zeroclaw_config::migration::migrate_legacy_workspace_to_default_agent(install_root)
            .expect("idempotent re-run must succeed");
    assert!(
        !ran_again,
        "second run is a no-op when the legacy dir is already gone"
    );
}

/// Multi-agent install: two agents on different memory backends
/// don't interfere. The schema validator rejects cross-backend
/// `read_memory_from` entries at config load; the runtime only ever
/// sees same-backend allowlists by the time the per-agent memory
/// factory builds its wrappers.
#[tokio::test]
async fn two_sqlite_agents_on_one_install_have_isolated_memory() {
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};

    let tmp = TempDir::new().unwrap();
    let install_root = tmp.path();
    let mut cfg = Config {
        data_dir: install_root.join("data"),
        config_path: install_root.join("config.toml"),
        ..Config::default()
    };
    std::fs::create_dir_all(&cfg.data_dir).unwrap();
    cfg.risk_profiles
        .insert("default".into(), RiskProfileConfig::default());
    cfg.providers.models.openrouter.insert(
        "default".to_string(),
        zeroclaw_config::schema::OpenRouterModelProviderConfig::default(),
    );
    for alias in ["alpha", "beta"] {
        cfg.agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                model_provider: "openrouter.default".into(),
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
    }

    // Build per-agent wrappers and store an attributable row from
    // each. Without an allowlist between them, neither sibling sees
    // the other's row.
    let alpha_mem = zeroclaw_memory::create_memory_for_agent(&cfg, "alpha", None)
        .await
        .expect("per-agent memory for alpha");
    let beta_mem = zeroclaw_memory::create_memory_for_agent(&cfg, "beta", None)
        .await
        .expect("per-agent memory for beta");

    alpha_mem
        .store(
            "alpha-key",
            "alpha owns this row",
            zeroclaw_memory::MemoryCategory::Core,
            None,
        )
        .await
        .expect("alpha store");
    beta_mem
        .store(
            "beta-key",
            "beta owns this row",
            zeroclaw_memory::MemoryCategory::Core,
            None,
        )
        .await
        .expect("beta store");

    // Alpha cannot see beta's row through the wrapper's allowlist
    // filter (read_memory_from is empty by default).
    let alpha_recall = alpha_mem
        .recall("beta-key", 10, None, None, None)
        .await
        .expect("alpha recall");
    assert!(
        !alpha_recall.iter().any(|e| e.key == "beta-key"),
        "alpha must not see beta-attributed rows when read_memory_from is empty"
    );

    // Symmetric: beta cannot see alpha's row.
    let beta_recall = beta_mem
        .recall("alpha-key", 10, None, None, None)
        .await
        .expect("beta recall");
    assert!(
        !beta_recall.iter().any(|e| e.key == "alpha-key"),
        "beta must not see alpha-attributed rows when read_memory_from is empty"
    );

    // Each can recall its own row.
    let alpha_self = alpha_mem
        .recall("alpha-key", 10, None, None, None)
        .await
        .expect("alpha self-recall");
    assert!(
        alpha_self.iter().any(|e| e.key == "alpha-key"),
        "agent must always recall its own rows"
    );
}

// e2e peer-group test removed in the channel-type fix (old test asserted
// alias-binding semantics, which was the bug). New test pending.
