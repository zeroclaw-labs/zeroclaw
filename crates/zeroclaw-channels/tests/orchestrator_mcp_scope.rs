//! Regression coverageat the channels-orchestrator MCP
//! connection site (`crates/zeroclaw-channels/src/orchestrator/mod.rs`,
//! search for `mcp_servers_for_agent`).

use std::collections::HashMap;

use zeroclaw_config::schema::{
    AliasedAgentConfig, Config, McpBundleConfig, McpServerConfig, McpTransport, RiskProfileConfig,
};

/// Build a two-agent config: `granted` (has bundle `b1`), `unscoped`
/// (no bundles). The server `remote` is configured globally.
fn make_two_agent_config() -> Config {
    let mut providers = zeroclaw_config::providers::Providers::default();
    {
        let base = providers
            .models
            .ensure("openai", "test-provider")
            .expect("`openai` slot must exist");
        base.api_key = Some("test-key".into());
        base.model = Some("test-model".into());
        base.uri = Some("http://127.0.0.1:1".into());
    }

    let mut risk_profiles = HashMap::new();
    risk_profiles.insert("test-profile".to_string(), RiskProfileConfig::default());

    let mut agents = HashMap::new();
    agents.insert(
        "granted".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "openai.test-provider".into(),
            risk_profile: "test-profile".into(),
            mcp_bundles: vec!["b1".into()],
            ..Default::default()
        },
    );
    agents.insert(
        "unscoped".to_string(),
        AliasedAgentConfig {
            enabled: true,
            model_provider: "openai.test-provider".into(),
            risk_profile: "test-profile".into(),
            mcp_bundles: Vec::new(),
            ..Default::default()
        },
    );

    let mut config = Config {
        providers,
        agents,
        risk_profiles,
        ..Config::default()
    };
    config.mcp.enabled = true;
    config.mcp.servers = vec![McpServerConfig {
        name: "remote".into(),
        transport: McpTransport::Http,
        url: Some("http://127.0.0.1:1".into()),
        ..Default::default()
    }];
    config.mcp_bundles.insert(
        "b1".into(),
        McpBundleConfig {
            servers: vec!["remote".into()],
            exclude: vec![],
        },
    );
    config
}

#[test]
fn resolver_grants_only_to_granted_agent_under_two_agent_config() {
    // The orchestrator now calls `config.mcp_servers_for_agent(alias)`.
    // Pin its semantics here: `granted` gets `remote`, `unscoped` gets
    // zero. If this flips, the orchestrator'sfix has regressed
    // at the contract level.
    let config = make_two_agent_config();

    let granted: Vec<String> = config
        .mcp_servers_for_agent("granted")
        .into_iter()
        .map(|s| s.name)
        .collect();
    assert_eq!(granted, vec!["remote"], "granted agent must get `remote`");

    assert!(
        config.mcp_servers_for_agent("unscoped").is_empty(),
        "unscoped agent must get zero servers (omission is not a grant)"
    );

    assert!(
        config.mcp_servers_for_agent("ghost-agent").is_empty(),
        "an unknown agent must get zero servers"
    );
}

/// Compile-time witness: the symbol the orchestrator depends on must
/// exist and have the right shape. If a future refactor removes or
/// renames `Config::mcp_servers_for_agent`, this file stops compiling
/// and signals reviewers to update the orchestrator call site too.
#[allow(dead_code)]
fn _mcp_servers_for_agent_witness(config: &Config, alias: &str) -> Vec<McpServerConfig> {
    config.mcp_servers_for_agent(alias)
}
