//! Regression coverage for resolving MCP server grants from agent bundles.

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
    let config = make_two_agent_config();

    let granted_servers: Vec<McpServerConfig> = config.mcp_servers_for_agent("granted");
    let granted: Vec<String> = granted_servers
        .into_iter()
        .map(|server| server.name)
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
