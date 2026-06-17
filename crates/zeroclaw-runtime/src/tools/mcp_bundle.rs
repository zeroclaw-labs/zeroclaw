/// Resolve the set of MCP servers that should be connected for a given agent,
/// applying the agent's `mcp_bundles` allowlist/exclude rules.
///
/// Mirrors `skills/mod.rs:load_skills_for_agent_from_config` — takes `agent_alias`
/// and resolves the agent internally so call sites stay clean.
///
/// Semantics:
/// - agent alias not in config — return all servers (unknown agent gets full access)
/// - `agent.mcp_bundles` is empty — return all servers (backward-compat)
/// - otherwise — union the `servers` lists from each named bundle, subtract
///   every name in any bundle's `exclude` list, and return only the matching
///   `McpServerConfig` entries
// THREE production call sites MUST route through this helper:
//   1. crates/zeroclaw-runtime/src/agent/loop_.rs — main agent turn loop (run)
//   2. crates/zeroclaw-runtime/src/agent/loop_.rs — persistent-message loop (process_message)
//   3. crates/zeroclaw-runtime/src/agent/agent.rs — from_config_with_session_cwd_and_mcp_approval_mode
// If any site is reverted to `&config.mcp.servers`, mcp_bundles scoping will silently stop
// working — the unit tests below will not catch that regression because they test this helper directly.
pub fn resolve_mcp_servers_for_agent(
    config: &zeroclaw_config::schema::Config,
    agent_alias: &str,
) -> Vec<zeroclaw_config::schema::McpServerConfig> {
    let agent = match config.agent(agent_alias) {
        Some(a) => a,
        None => return config.mcp.servers.clone(),
    };

    if agent.mcp_bundles.is_empty() {
        return config.mcp.servers.clone();
    }

    let mut included: std::collections::HashSet<String> = Default::default();
    let mut excluded: std::collections::HashSet<String> = Default::default();

    for bundle_alias in &agent.mcp_bundles {
        match config.mcp_bundles.get(bundle_alias) {
            Some(bundle) => {
                included.extend(bundle.servers.iter().cloned());
                excluded.extend(bundle.exclude.iter().cloned());
            }
            None => {
                ::zeroclaw_log::record!(
                    WARN,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                        .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                        .with_attrs(
                            ::serde_json::json!({ "agent": agent_alias, "alias": bundle_alias })
                        ),
                    "mcp_bundles: unknown bundle alias — skipping"
                );
            }
        }
    }

    config
        .mcp
        .servers
        .iter()
        .filter(|s| included.contains(&s.name) && !excluded.contains(&s.name))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zeroclaw_config::schema::{
        AliasedAgentConfig, Config, McpBundleConfig, McpConfig, McpServerConfig,
    };

    fn make_stdio_server(name: &str) -> McpServerConfig {
        McpServerConfig {
            name: name.to_string(),
            transport: zeroclaw_config::schema::McpTransport::Stdio,
            command: "echo".to_string(),
            ..Default::default()
        }
    }

    fn make_config(
        servers: Vec<McpServerConfig>,
        bundles: HashMap<String, McpBundleConfig>,
        agents: HashMap<String, AliasedAgentConfig>,
    ) -> Config {
        Config {
            mcp: McpConfig {
                servers,
                enabled: true,
                ..Default::default()
            },
            mcp_bundles: bundles,
            agents,
            ..Default::default()
        }
    }

    fn make_agent(bundles: Vec<&str>) -> AliasedAgentConfig {
        AliasedAgentConfig {
            mcp_bundles: bundles.into_iter().map(String::from).collect(),
            ..Default::default()
        }
    }

    fn bundle(servers: Vec<&str>, exclude: Vec<&str>) -> McpBundleConfig {
        McpBundleConfig {
            servers: servers.into_iter().map(String::from).collect(),
            exclude: exclude.into_iter().map(String::from).collect(),
        }
    }

    #[test]
    fn unknown_agent_returns_all_servers() {
        let config = make_config(
            vec![make_stdio_server("a"), make_stdio_server("b")],
            HashMap::new(),
            HashMap::new(),
        );
        let result = resolve_mcp_servers_for_agent(&config, "ghost");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn empty_mcp_bundles_returns_all_servers() {
        let config = make_config(
            vec![make_stdio_server("a"), make_stdio_server("b")],
            HashMap::new(),
            HashMap::from([("alice".to_string(), make_agent(vec![]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn unknown_bundle_alias_warns_and_returns_empty() {
        let config = make_config(
            vec![make_stdio_server("a")],
            HashMap::new(),
            HashMap::from([("alice".to_string(), make_agent(vec!["nonexistent"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        assert!(result.is_empty());
    }

    #[test]
    fn single_bundle_includes_listed_servers() {
        let config = make_config(
            vec![
                make_stdio_server("a"),
                make_stdio_server("b"),
                make_stdio_server("c"),
            ],
            HashMap::from([("web".to_string(), bundle(vec!["a", "b"], vec![]))]),
            HashMap::from([("alice".to_string(), make_agent(vec!["web"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        let names: Vec<_> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a", "b"]);
    }

    #[test]
    fn exclude_removes_server_from_bundle() {
        let config = make_config(
            vec![make_stdio_server("a"), make_stdio_server("b")],
            HashMap::from([("web".to_string(), bundle(vec!["a", "b"], vec!["b"]))]),
            HashMap::from([("alice".to_string(), make_agent(vec!["web"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        let names: Vec<_> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn multiple_bundles_union_minus_excludes() {
        let config = make_config(
            vec![
                make_stdio_server("a"),
                make_stdio_server("b"),
                make_stdio_server("c"),
                make_stdio_server("d"),
            ],
            HashMap::from([
                ("bundle1".to_string(), bundle(vec!["a", "b"], vec![])),
                ("bundle2".to_string(), bundle(vec!["c", "d"], vec!["d"])),
            ]),
            HashMap::from([("alice".to_string(), make_agent(vec!["bundle1", "bundle2"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        let mut names: Vec<_> = result.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, vec!["a", "b", "c"]);
    }

    #[test]
    fn bundle_server_not_in_config_is_silently_absent() {
        let config = make_config(
            vec![make_stdio_server("a")],
            HashMap::from([("web".to_string(), bundle(vec!["a", "phantom"], vec![]))]),
            HashMap::from([("alice".to_string(), make_agent(vec!["web"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        let names: Vec<_> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["a"]);
    }

    #[test]
    fn exclude_takes_priority_over_include() {
        let config = make_config(
            vec![make_stdio_server("a")],
            HashMap::from([
                ("inc".to_string(), bundle(vec!["a"], vec![])),
                ("exc".to_string(), bundle(vec![], vec!["a"])),
            ]),
            HashMap::from([("alice".to_string(), make_agent(vec!["inc", "exc"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "alice");
        assert!(result.is_empty());
    }

    // Regression guard for the agent.rs / from_config path.
    //
    // `from_config_with_session_cwd_and_mcp_approval_mode` passes `config` and
    // `agent_alias` directly to `resolve_mcp_servers_for_agent` — we can't call
    // `from_config` in unit tests (requires a live model provider), so we verify
    // the resolver contract directly using the same config shape that path would
    // supply.  A revert of the agent.rs wiring makes this test vacuously pass
    // (the site would call connect_all with all servers, not the filtered slice),
    // but the protective comment above names agent.rs explicitly so reviewers and
    // grep catch any revert.
    #[test]
    fn from_config_path_agent_with_bundle_excludes_sensitive_server() {
        // config.mcp.servers has two entries: "public" and "sensitive"
        // agent "bob" has mcp_bundles=["safe_only"] which includes only "public"
        let config = make_config(
            vec![make_stdio_server("public"), make_stdio_server("sensitive")],
            HashMap::from([("safe_only".to_string(), bundle(vec!["public"], vec![]))]),
            HashMap::from([("bob".to_string(), make_agent(vec!["safe_only"]))]),
        );
        let result = resolve_mcp_servers_for_agent(&config, "bob");
        let names: Vec<_> = result.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["public"]);
        assert!(!names.contains(&"sensitive"));
    }
}
