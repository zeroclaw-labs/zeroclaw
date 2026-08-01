//! End-to-end tests for the multi-agent runtime.

use tempfile::TempDir;

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

    let report = zeroclaw_config::schema::v2::migrate_v2_to_v3_install_filesystem(install_root)
        .expect("migration must succeed on populated legacy install");
    assert!(
        report.entries_relocated > 0 && report.backup_dir.is_some(),
        "populated legacy install → split migration runs"
    );

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

    // Idempotent re-run: legacy gone → no-op (no backup, nothing moved).
    let report_again =
        zeroclaw_config::schema::v2::migrate_v2_to_v3_install_filesystem(install_root)
            .expect("idempotent re-run must succeed");
    assert!(
        report_again.backup_dir.is_none() && report_again.entries_relocated == 0,
        "second run is a no-op when the legacy dir is already gone"
    );
}

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

#[tokio::test]
async fn peer_group_routes_messages_only_within_resolved_peer_set() {
    use serde_json::json;
    use std::sync::Arc;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig, PeerUsername};
    use zeroclaw_config::providers::ChannelRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
    use zeroclaw_runtime::peers::resolve_peer_set;
    use zeroclaw_runtime::tools::SendMessageToPeerTool;

    let mut cfg = Config::default();
    cfg.risk_profiles
        .insert("research-floor".into(), RiskProfileConfig::default());
    for alias in ["alpha", "beta", "gamma"] {
        let mut agent = AliasedAgentConfig {
            risk_profile: "research-floor".into(),
            ..AliasedAgentConfig::default()
        };
        agent.channels.push(ChannelRef::from("telegram.prod"));
        cfg.agents.insert(alias.to_string(), agent);
    }
    cfg.peer_groups.insert(
        "research".into(),
        PeerGroupConfig {
            // Legacy channel type-wide binding remains accepted.
            channel: "telegram".into(),
            agents: vec![AgentAlias::from("alpha"), AgentAlias::from("beta")],
            external_peers: vec![PeerUsername::from("operator")],
            ignore: vec![],
            ..Default::default()
        },
    );

    let alpha_peers = resolve_peer_set(&cfg, "alpha");
    assert!(
        alpha_peers.is_known_peer("telegram", "beta"),
        "alpha must recognize peer beta for outbound dispatch on type `telegram`"
    );
    assert!(
        alpha_peers.is_known_peer("telegram", "@Operator"),
        "alpha must recognize external peer (case + @ normalized) for outbound on type `telegram`"
    );
    assert!(
        !alpha_peers.is_known_peer("telegram", "gamma"),
        "alpha must NOT recognize gamma for outbound — gamma is not on the peer group"
    );

    let gamma_peers = resolve_peer_set(&cfg, "gamma");
    assert_eq!(
        gamma_peers,
        zeroclaw_runtime::peers::ResolvedPeers::default(),
        "gamma is on no peer group; resolved set is empty"
    );

    let cfg = Arc::new(cfg);
    let tool = SendMessageToPeerTool::new(cfg.clone(), "alpha");

    let to_gamma = tool
        .execute(json!({
            "channel": "telegram.prod",
            "target": "gamma",
            "message": "hi"
        }))
        .await
        .expect("execute returns Ok with structured failure");
    assert!(!to_gamma.success, "send to non-peer must be rejected");
    assert!(
        to_gamma
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("resolved peer set"),
        "rejection must name the peer-set check (not a delivery failure), got: {:?}",
        to_gamma.error
    );

    let to_beta = tool
        .execute(json!({
            "channel": "telegram.prod",
            "target": "beta",
            "message": "hi"
        }))
        .await
        .expect("execute returns Ok");
    assert!(
        to_beta.success,
        "in-process peer delivery must return success without blocking the sender, got: {to_beta:?}"
    );
    assert!(
        to_beta.output.contains("in-process"),
        "in-process delivery output must name its routing path so the agent can reason about delivery semantics, got: {:?}",
        to_beta.output
    );
}

#[tokio::test]
async fn peer_group_dotted_channel_refs_remain_alias_scoped_for_dispatch() {
    use serde_json::json;
    use std::sync::Arc;
    use zeroclaw_api::tool::Tool;
    use zeroclaw_config::multi_agent::{AgentAlias, PeerGroupConfig};
    use zeroclaw_config::providers::ChannelRef;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
    use zeroclaw_runtime::peers::resolve_peer_set;
    use zeroclaw_runtime::tools::SendMessageToPeerTool;

    let mut cfg = Config::default();
    cfg.risk_profiles
        .insert("research-floor".into(), RiskProfileConfig::default());
    for alias in ["alpha", "beta"] {
        let mut agent = AliasedAgentConfig {
            risk_profile: "research-floor".into(),
            ..AliasedAgentConfig::default()
        };
        agent.channels.push(ChannelRef::from("telegram.prod"));
        agent.channels.push(ChannelRef::from("telegram.dev"));
        cfg.agents.insert(alias.to_string(), agent);
    }
    cfg.peer_groups.insert(
        "research-prod".into(),
        PeerGroupConfig {
            channel: "telegram.prod".into(),
            agents: vec![AgentAlias::from("alpha"), AgentAlias::from("beta")],
            external_peers: vec![],
            ignore: vec![],
            ..Default::default()
        },
    );

    let alpha_peers = resolve_peer_set(&cfg, "alpha");
    assert!(alpha_peers.is_known_peer("telegram.prod", "beta"));
    assert!(
        !alpha_peers.is_known_peer("telegram.dev", "beta"),
        "alias-scoped peer groups must not broaden to sibling channel aliases"
    );

    let cfg = Arc::new(cfg);
    let tool = SendMessageToPeerTool::new(cfg, "alpha");
    let prod = tool
        .execute(json!({
            "channel": "telegram.prod",
            "target": "beta",
            "message": "hi"
        }))
        .await
        .expect("execute returns Ok");
    assert!(
        prod.success,
        "exact alias dispatch should succeed: {prod:?}"
    );

    let dev = tool
        .execute(json!({
            "channel": "telegram.dev",
            "target": "beta",
            "message": "hi"
        }))
        .await
        .expect("execute returns Ok with structured failure");
    assert!(
        !dev.success,
        "sibling alias dispatch must not inherit telegram.prod peers"
    );
}
