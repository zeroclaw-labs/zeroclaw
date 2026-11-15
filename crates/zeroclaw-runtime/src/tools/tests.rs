#[cfg(test)]
pub(crate) use super::*;
use tempfile::TempDir;
use zeroclaw_config::schema::{BrowserConfig, Config, MemoryConfig};

#[tokio::test]
async fn mcp_capability_tools_respect_policy() {
    use zeroclaw_tools::tool_search::ToolAccessPolicy;
    let registry = std::sync::Arc::new(McpRegistry::connect_all(&[]).await.unwrap());

    // No policy → both tools present.
    let both = build_mcp_capability_tools(&registry, None);
    let names: Vec<_> = both.iter().map(|t| t.name().to_string()).collect();
    assert!(names.contains(&"mcp_resources".to_string()));
    assert!(names.contains(&"mcp_prompts".to_string()));

    // Deny mcp_prompts → only mcp_resources present.
    let policy = ToolAccessPolicy::from_security(
        None,
        Some(&["mcp_prompts".to_string()]),
        None,
        zeroclaw_config::autonomy::McpDiscoveredToolPolicy::default(),
    );
    let one = build_mcp_capability_tools(&registry, policy.as_ref());
    let names: Vec<_> = one.iter().map(|t| t.name().to_string()).collect();
    assert!(names.contains(&"mcp_resources".to_string()));
    assert!(!names.contains(&"mcp_prompts".to_string()));
}

fn test_config(tmp: &TempDir) -> Config {
    Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    }
}

#[test]
fn email_factory_respects_compile_feature_and_channel_activation() {
    for enabled in [None, Some(false), Some(true)] {
        let tmp = TempDir::new().unwrap();
        let mut cfg = test_config(&tmp);
        cfg.composition = Some(zeroclaw_config::composition::Composition::Full);
        if let Some(enabled) = enabled {
            cfg.channels.email.insert(
                "fixture".into(),
                zeroclaw_config::scattered_types::EmailConfig {
                    enabled,
                    imap_host: "mail.invalid".into(),
                    ..Default::default()
                },
            );
        }
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let mem_cfg = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let mem: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
        let risk_profile = zeroclaw_config::schema::RiskProfileConfig {
            sandbox_enabled: Some(false),
            ..Default::default()
        };
        let tools = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &risk_profile,
            "test-agent",
            mem,
            &BrowserConfig::default(),
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|tool| tool.name()).collect();
        let expected = cfg!(feature = "email-tools") && enabled == Some(true);
        assert_eq!(names.contains(&"email_search"), expected, "{enabled:?}");
        assert_eq!(names.contains(&"email_read"), expected, "{enabled:?}");
        assert!(names.contains(&"file_read"));
    }
}

#[test]
fn default_tools_has_expected_count() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    assert_eq!(tools.len(), 6);
}

/// Discrimination guard for the retired SOP run side: the
/// legacy agent-facing run tools must never re-enter the registry. Run
/// truth is Tachi-side (procedure_v1 seam); definitions have no tool
/// surface here.
#[test]
fn sop_run_tools_stay_retired_from_the_registry() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig {
        enabled: false,
        allowed_domains: vec![],
        session_name: None,
        ..BrowserConfig::default()
    };
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let cfg = test_config(&tmp);

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    let retired_sop_tools = [
        "sop_list",
        "sop_execute",
        "sop_advance",
        "sop_approve",
        "sop_status",
        "sop_workshop",
    ];
    for name in &retired_sop_tools {
        assert!(
            !names.contains(name),
            "legacy SOP run tool '{name}' is retired; the registry must not re-admit it"
        );
    }
}

#[tokio::test]
async fn retired_tools_stay_absent_without_removing_human_channel_tools() {
    // Wall 2 raw-launcher retirement: the Parent-visible raw harness/vendor
    // launch tools are retired together with their config sections. No
    // registry path may re-admit them; harness execution goes through the
    // typed subagent/Tachi paths.
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy {
        autonomy: crate::security::AutonomyLevel::Full,
        workspace_dir: tmp.path().to_path_buf(),
        ..SecurityPolicy::default()
    });
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
    let browser = BrowserConfig {
        enabled: false,
        ..BrowserConfig::default()
    };
    let mut cfg = test_config(&tmp);
    cfg.composition = Some(zeroclaw_config::composition::Composition::Full);
    let risk = zeroclaw_config::schema::RiskProfileConfig {
        sandbox_enabled: Some(false),
        sandbox_backend: Some("none".to_string()),
        ..zeroclaw_config::schema::RiskProfileConfig::default()
    };

    let registry = all_tools_with_runtime(
        Arc::new(cfg.clone()),
        &security,
        &risk,
        "test-agent",
        Arc::new(zeroclaw_platform::NativeRuntime::new()),
        mem,
        &browser,
        &zeroclaw_config::schema::HttpRequestConfig::default(),
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
        None,
        None,
        None,
    );
    let names: Vec<&str> = registry.tools.iter().map(|tool| tool.name()).collect();

    for tool_name in [
        "claude_code",
        "claude_code_runner",
        "codex_cli",
        "gemini_cli",
        "opencode_cli",
        "browser_delegate",
    ] {
        assert!(
            !names.contains(&tool_name),
            "retired raw launcher '{tool_name}' must not register under any composition"
        );
    }
    assert!(
        names.contains(&"shell"),
        "positive control: ordinary tools should still register"
    );
    assert!(
        !names.contains(&"send_via")
            && !registry
                .unfiltered_tool_arcs
                .iter()
                .any(|tool| tool.name() == "send_via"),
        "send_via has no turn-routing consumer and must not be offered or elevated"
    );
    assert!(names.contains(&"ask_user"));
    assert!(names.contains(&"escalate_to_human"));
    assert!(registry.ask_user_handle.is_some());
    assert!(registry.escalate_handle.is_some());
}

#[test]
fn shared_store_tools_open_data_dir_not_per_agent_workspace() {
    let tmp = TempDir::new().unwrap();
    let data_dir = tmp.path().join("data"); // shared store (writers' dir)
    let workspace_dir = tmp.path().join("agent-ws"); // per-agent, intentionally distinct
    std::fs::create_dir_all(&data_dir).unwrap();
    std::fs::create_dir_all(&workspace_dir).unwrap();

    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());
    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let web = zeroclaw_config::schema::WebFetchConfig::default();
    let risk = zeroclaw_config::schema::RiskProfileConfig::default();

    // root_config: shared data_dir + a Discord alias that archives (this is
    // what gates discord_search registration).
    let mut root_config = test_config(&tmp);
    root_config.data_dir = data_dir.clone();
    root_config.channels.discord.insert(
        "oracle".to_string(),
        zeroclaw_config::schema::DiscordConfig {
            archive: true,
            ..Default::default()
        },
    );

    // `config` (arg 1) carries the canonical shared data_dir — exactly how
    // the production callers pass it (a clone of the runtime config).
    let config = Config {
        data_dir: data_dir.clone(),
        ..Config::default()
    };

    let tools = all_tools_with_runtime(
        Arc::new(config),
        &security,
        &risk,
        "test-agent",
        Arc::new(NativeRuntime::new()),
        mem,
        &browser,
        &http,
        &web,
        workspace_dir.as_path(), // DIFFERENT from data_dir
        &HashMap::new(),
        None,
        &root_config,
        None,
        None,
        None,
        None,
    )
    .tools;

    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(
        names.contains(&"discord_search"),
        "discord_search must register when a Discord alias archives"
    );
    assert!(
        names.iter().any(|n| n.starts_with("sessions")),
        "session tools must register"
    );

    // The fix: both stores open under the shared data_dir, never the
    // per-agent workspace. Pre-fix the readers created `memory/discord.db`
    // and `sessions/sessions.db` under the workspace_dir.
    assert!(
        !workspace_dir.join("memory").exists(),
        "discord_search must not open/create a store under the per-agent workspace_dir"
    );
    assert!(
        !workspace_dir.join("sessions").exists(),
        "session tools must not open/create a store under the per-agent workspace_dir"
    );
}

/// A runtime that reports an ephemeral workspace (no host persistence) while
/// delegating real shell execution to `NativeRuntime`. Used to exercise the
/// registration wiring of `has_filesystem_access()` -> `persistent_writes`.
struct EphemeralRuntime(NativeRuntime);

impl RuntimeAdapter for EphemeralRuntime {
    fn name(&self) -> &str {
        "ephemeral-test"
    }
    fn has_shell_access(&self) -> bool {
        true
    }
    fn has_filesystem_access(&self) -> bool {
        false
    }
    fn storage_path(&self) -> std::path::PathBuf {
        std::env::temp_dir()
    }
    fn supports_long_running(&self) -> bool {
        false
    }
    fn build_shell_command(
        &self,
        command: &str,
        workspace_dir: &std::path::Path,
    ) -> anyhow::Result<tokio::process::Command> {
        self.0.build_shell_command(command, workspace_dir)
    }
}

#[tokio::test]
async fn registered_tools_warn_or_block_on_ephemeral_runtime() {
    let tmp = TempDir::new().unwrap();
    tokio::fs::write(tmp.path().join("notes.txt"), "data")
        .await
        .unwrap();
    let security = Arc::new(SecurityPolicy {
        autonomy: crate::security::AutonomyLevel::Supervised,
        max_actions_per_hour: 100,
        workspace_dir: tmp.path().to_path_buf(),
        ..SecurityPolicy::default()
    });
    let runtime: Arc<dyn RuntimeAdapter> = Arc::new(EphemeralRuntime(NativeRuntime::new()));
    let tools = default_tools_with_runtime(security, runtime);
    let by_name = |n: &str| tools.iter().find(|t| t.name() == n).unwrap();

    // shell: warns on the executed command.
    let r = by_name("shell")
        .execute(serde_json::json!({"command": "echo hi"}))
        .await
        .unwrap();
    assert!(
        r.output.contains("EPHEMERAL WORKSPACE"),
        "shell must warn, got: {}",
        r.output
    );

    // file_read: warns on a successful text read.
    let r = by_name("file_read")
        .execute(serde_json::json!({"path": "notes.txt"}))
        .await
        .unwrap();
    assert!(
        r.success && r.output.contains("EPHEMERAL WORKSPACE"),
        "file_read must warn, got: {r:?}"
    );

    // file_edit: warns on a successful edit.
    let r = by_name("file_edit")
        .execute(serde_json::json!({"path": "notes.txt", "old_string": "data", "new_string": "x"}))
        .await
        .unwrap();
    assert!(
        r.success && r.output.contains("EPHEMERAL WORKSPACE"),
        "file_edit must warn, got: {r:?}"
    );

    // file_write: refuses outright (does not warn-and-write).
    let r = by_name("file_write")
        .execute(serde_json::json!({"path": "new.txt", "content": "x"}))
        .await
        .unwrap();
    assert!(
        !r.success,
        "file_write must refuse on ephemeral, got: {r:?}"
    );
    assert!(
        r.error
            .as_deref()
            .unwrap_or("")
            .contains("ephemeral workspace"),
        "file_write error must name the cause, got: {:?}",
        r.error
    );
    assert!(
        !tmp.path().join("new.txt").exists(),
        "file_write must not write anything on ephemeral"
    );
}

#[test]
fn all_tools_excludes_browser_when_disabled() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig {
        enabled: false,
        allowed_domains: vec!["example.com".into()],
        session_name: None,
        ..BrowserConfig::default()
    };
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let cfg = test_config(&tmp);

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"browser_open"));
    assert!(names.contains(&"schedule"));
    // Operator/admin tools are never part of the model surface.
    assert!(!names.contains(&"model_routing_config"));
    assert!(!names.contains(&"proxy_config"));
}

#[test]
fn minimal_composition_cuts_registry_to_membership() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig {
        enabled: false,
        allowed_domains: vec!["example.com".into()],
        session_name: None,
        ..BrowserConfig::default()
    };
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.composition = Some(zeroclaw_config::composition::Composition::Minimal);
    // Explicitly enabled non-members must not widen the minimal profile.
    cfg.browser.enabled = true;

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    // `read_skill` and `tool_search` are conditional members (compact
    // skills mode / deferred MCP respectively); in this default-config
    // fixture they are not registered at all, which the totality check
    // below covers from the other side.
    for member in [
        "shell",
        "file_read",
        "file_write",
        "file_edit",
        "glob_search",
        "content_search",
        "schedule",
        "reasoning_subagent",
    ] {
        assert!(
            names.contains(&member),
            "minimal profile must keep {member}, got: {names:?}"
        );
    }
    // The minimal composition fronts the V1 entrypoint; the legacy
    // `spawn_subagent` is retired on every composition.
    assert!(
        !names.contains(&"spawn_subagent"),
        "minimal profile must drop the retired spawn_subagent; got: {names:?}"
    );
    // Fail-closed totality: nothing outside the membership table may be
    // assembled under minimal, whatever flags enabled it.
    for name in &names {
        assert!(
            zeroclaw_config::composition::is_minimal_member(name),
            "non-member leaked into minimal assembly: {name}"
        );
    }
    assert!(!names.contains(&"model_routing_config"));
    assert!(!names.contains(&"proxy_config"));
    assert!(!names.contains(&"claude_code"));
}

#[test]
fn absent_composition_keeps_full_assembly() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig {
        enabled: false,
        allowed_domains: vec!["example.com".into()],
        session_name: None,
        ..BrowserConfig::default()
    };
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let cfg = test_config(&tmp);
    assert!(
        cfg.composition.is_none(),
        "test_config must not set a composition"
    );

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    // Absent field resolves as full: today's default assembly minus the
    // retired operator/admin tools, which no composition may re-admit.
    assert!(!names.contains(&"model_routing_config"));
    assert!(!names.contains(&"proxy_config"));
    // The legacy full-Parent-inheritance spawn entrypoint is retired
    // (spawn_subagent wall): no composition registers it, and the
    // retired-name guard keeps plugins from claiming it.
    assert!(
        !names.contains(&"spawn_subagent"),
        "full composition must not register the retired spawn_subagent; got: {names:?}"
    );
    assert!(
        names.contains(&"reasoning_subagent"),
        "full composition must carry the V1 reasoning_subagent; got: {names:?}"
    );
}

#[test]
fn all_tools_includes_browser_when_enabled() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig {
        enabled: true,
        allowed_domains: vec!["example.com".into()],
        session_name: None,
        ..BrowserConfig::default()
    };
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let cfg = test_config(&tmp);

    let tools = all_tools(
        Arc::new(Config::default()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"browser_open"));
    assert!(names.contains(&"content_search"));
    // Operator/admin tools are never part of the model surface.
    assert!(!names.contains(&"model_routing_config"));
    assert!(!names.contains(&"proxy_config"));
}

#[test]
fn default_tools_names() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"shell"));
    assert!(names.contains(&"file_read"));
    assert!(names.contains(&"file_write"));
    assert!(names.contains(&"file_edit"));
    assert!(names.contains(&"glob_search"));
    assert!(names.contains(&"content_search"));
}

#[test]
fn default_tools_names_match_api_metadata() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert_eq!(names.as_slice(), zeroclaw_api::tool::DEFAULT_TOOL_NAMES);
}

#[test]
fn default_tools_all_have_descriptions() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    for tool in &tools {
        assert!(
            !tool.description().is_empty(),
            "Tool {} has empty description",
            tool.name()
        );
    }
}

#[test]
fn default_tools_all_have_schemas() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    for tool in &tools {
        let schema = tool.parameters_schema();
        assert!(
            schema.is_object(),
            "Tool {} schema is not an object",
            tool.name()
        );
        assert!(
            schema["properties"].is_object(),
            "Tool {} schema has no properties",
            tool.name()
        );
    }
}

#[test]
fn tool_spec_generation() {
    let security = Arc::new(SecurityPolicy::default());
    let tools = default_tools(security);
    for tool in &tools {
        let spec = tool.spec();
        assert_eq!(spec.name, tool.name());
        assert_eq!(spec.description, tool.description());
        assert!(spec.parameters.is_object());
    }
}

#[test]
fn tool_result_serde() {
    let result = ToolResult {
        success: true,
        output: "hello".into(),
        error: None,
    };
    let json = serde_json::to_string(&result).unwrap();
    let parsed: ToolResult = serde_json::from_str(&json).unwrap();
    assert!(parsed.success);
    assert_eq!(parsed.output, "hello");
    assert!(parsed.error.is_none());
}

#[test]
fn tool_result_with_error_serde() {
    let result = ToolResult {
        success: false,
        output: ToolOutput::default(),
        error: Some("boom".into()),
    };
    let json = serde_json::to_string(&result).unwrap();
    let parsed: ToolResult = serde_json::from_str(&json).unwrap();
    assert!(!parsed.success);
    assert_eq!(parsed.error.as_deref(), Some("boom"));
}

#[test]
fn tool_spec_serde() {
    let spec = ToolSpec::new("test", "A test tool", serde_json::json!({"type": "object"}));
    let json = serde_json::to_string(&spec).unwrap();
    let parsed: ToolSpec = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed.name, "test");
    assert_eq!(parsed.description, "A test tool");
}

#[test]
fn delegate_stays_absent_from_every_registry() {
    // Wall 1: the legacy full-parent-inheritance delegation tool is
    // retired. Neither an agents-configured full-composition registry nor
    // an agents-less one may surface it, and the retired-name guard keeps
    // plugins from claiming the name. The V1 SubAgent entrypoint
    // (`reasoning_subagent`) is the only spawn-capable model-visible
    // tool; the legacy `spawn_subagent` retired with its wall.
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let cfg = test_config(&tmp);

    let mut agents = HashMap::new();
    agents.insert(
        "researcher".to_string(),
        AliasedAgentConfig {
            model_provider: "ollama.researcher".into(),
            ..Default::default()
        },
    );

    for (label, agents) in [
        ("agents configured", agents.clone()),
        ("no agents", HashMap::new()),
    ] {
        let tools = all_tools(
            Arc::new(Config::default()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem.clone(),
            &browser,
            &http,
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &agents,
            Some("delegate-test-credential"),
            &cfg,
            None,
        )
        .tools;
        let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
        assert!(
            !names.contains(&"delegate"),
            "delegate must not be registered ({label}); got {names:?}"
        );
    }
}

// ── Unified lineage (SA-9/SA-10/SA-11): the GREEN side of the
// census zig-zag red→green pair. The RED evidence (master, where the
// rebuilt registry minted depth 0 and sailed past the depth gate) is
// captured verbatim in the PR body; these tests pin the closed
// behavior.

fn lineage_agents() -> HashMap<String, AliasedAgentConfig> {
    let mut agents = HashMap::new();
    for alias in ["parent-agent", "child-target"] {
        agents.insert(
            alias.to_string(),
            AliasedAgentConfig {
                risk_profile: "default".into(),
                ..AliasedAgentConfig::default()
            },
        );
    }
    agents
}

fn lineage_registry_config() -> Config {
    let mut config = Config::default();
    let risk = zeroclaw_config::schema::RiskProfileConfig::default();
    config.risk_profiles.insert("default".to_string(), risk);
    config.agents = lineage_agents();
    config
}

#[test]
fn registry_binds_the_agents_advisor_into_reasoning_subagent() {
    let security = Arc::new(SecurityPolicy::default());
    let mut cfg = lineage_registry_config();

    let plain =
        reasoning_spawn_tool_for_registry(&Arc::new(cfg.clone()), "parent-agent", &security, None);
    assert_eq!(plain.advisor_ref(), None);

    let agent = cfg.agents.get_mut("parent-agent").unwrap();
    agent.advisor = Some(zeroclaw_config::advisor::AdvisorTarget::Model(
        "anthropic.opus".into(),
    ));
    let advised =
        reasoning_spawn_tool_for_registry(&Arc::new(cfg.clone()), "parent-agent", &security, None);
    assert_eq!(advised.advisor_ref(), Some("anthropic.opus"));
    assert!(advised.description().contains("advisor model"));

    // A harness target never binds (validation refuses it; the registry
    // does not guess).
    cfg.agents.get_mut("parent-agent").unwrap().advisor = Some(
        zeroclaw_config::advisor::AdvisorTarget::Harness("codex".into()),
    );
    let harness =
        reasoning_spawn_tool_for_registry(&Arc::new(cfg.clone()), "parent-agent", &security, None);
    assert_eq!(harness.advisor_ref(), None);
}

#[test]
fn registry_keeps_root_advisor_and_bridge_snapshot_separate_from_runtime_config() {
    for bridge_enabled in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut root_config = test_config(&tmp);
        root_config.composition = Some(zeroclaw_config::composition::Composition::Full);
        root_config.agents.insert(
            "test-agent".into(),
            AliasedAgentConfig {
                advisor: Some(zeroclaw_config::advisor::AdvisorTarget::Model(
                    "anthropic.root-only".into(),
                )),
                ..Default::default()
            },
        );
        if bridge_enabled {
            root_config.gateway.bridges.insert(
                "root-only".into(),
                zeroclaw_config::schema::GatewayBridgeConfig::default(),
            );
        }

        // The runtime snapshot deliberately has another bridge and no advisor.
        // Sharing its Arc would silently change the retained root consumers.
        let mut runtime_config = root_config.clone();
        runtime_config.agents.get_mut("test-agent").unwrap().advisor = None;
        runtime_config.gateway.bridges.clear();
        runtime_config.gateway.bridges.insert(
            "runtime-only".into(),
            zeroclaw_config::schema::GatewayBridgeConfig::default(),
        );
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..SecurityPolicy::default()
        });
        let memory_config = MemoryConfig {
            backend: "markdown".into(),
            ..MemoryConfig::default()
        };
        let memory: Arc<dyn Memory> =
            Arc::from(zeroclaw_memory::create_memory(&memory_config, tmp.path(), None).unwrap());
        let risk = zeroclaw_config::schema::RiskProfileConfig {
            sandbox_enabled: Some(false),
            ..Default::default()
        };
        let tools = all_tools(
            Arc::new(runtime_config),
            &security,
            &risk,
            "test-agent",
            memory,
            &BrowserConfig::default(),
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &root_config,
            None,
        )
        .tools;

        let reasoning = tools
            .iter()
            .find(|tool| tool.name() == "reasoning_subagent")
            .unwrap();
        assert!(reasoning.description().contains("advisor model"));
        let notify = tools.iter().find(|tool| tool.name() == "notify");
        assert_eq!(notify.is_some(), bridge_enabled);
        if let Some(notify) = notify {
            assert_eq!(
                notify.parameters_schema()["properties"]["bridge"]["enum"],
                serde_json::json!(["root-only"])
            );
        }
    }
}

#[tokio::test]
async fn registry_rebuild_carries_spawn_lineage_and_cannot_reset_depth() {
    // The census zig-zag GREEN half: a registry built for a child
    // context whose lineage is at the depth cap (exactly what
    // `agent::run` builds for a spawned child of a depth-3 parent)
    // carries the ONE ledger through the rebuild. The behavioral
    // refusal at that depth is pinned on the surviving spawn
    // surface in `subagent_v1::tests`; here the rebuilt registry's
    // SHAPE is the discrimination: both retired spawn tools are
    // absent, the V1 entrypoint is present and inherits the depth.
    let tmp = TempDir::new().unwrap();
    let cfg = lineage_registry_config();
    let mut build_cfg = cfg.clone();
    build_cfg.data_dir = tmp.path().join("data");
    build_cfg.config_path = tmp.path().join("config.toml");
    let security = Arc::new(SecurityPolicy::for_agent(&build_cfg, "parent-agent").unwrap());
    let mem: Arc<dyn Memory> = Arc::from(
        zeroclaw_memory::create_memory(&MemoryConfig::default(), tmp.path(), None).unwrap(),
    );

    let at_cap = zeroclaw_api::subagent_v1::LineageRef::new_root(
        zeroclaw_api::subagent_v1::ParentRunRef::from_opaque("zigzag-root"),
    )
    .child()
    .child()
    .child(); // depth 3 = default cap

    // The lineage thread-through is discriminated at the construction
    // site: the helper the registry vec calls must carry the run's
    // lineage, so dropping the `.with_lineage` thread flips this red
    // (a lineage-None reasoning tool inside a child registry would
    // admit D1-forbidden spawns from depth > 0 contexts).
    let probe = reasoning_spawn_tool_for_registry(
        &Arc::new(build_cfg.clone()),
        "parent-agent",
        &security,
        Some(at_cap.clone()),
    );
    assert_eq!(
        probe
            .carried_lineage()
            .map(zeroclaw_api::subagent_v1::LineageRef::depth),
        Some(3),
        "the registry construction site must thread the run's spawn lineage \
         into the surviving spawn tool"
    );

    let built = all_tools_with_runtime(
        Arc::new(build_cfg),
        &security,
        &cfg.risk_profiles.get("default").cloned().unwrap(),
        "parent-agent",
        Arc::new(NativeRuntime::new()),
        mem,
        &BrowserConfig::default(),
        &zeroclaw_config::schema::HttpRequestConfig::default(),
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &cfg.agents,
        None,
        &lineage_registry_config(),
        None,
        None,
        Some(at_cap.clone()),
        None,
    );

    // The retired legacy spawn tools must not reappear in a rebuilt
    // registry: the zig-zag chain loses its legacy hops entirely.
    let names: Vec<String> = built.tools.iter().map(|t| t.name().to_string()).collect();
    assert!(
        !names.contains(&"delegate".to_string()),
        "delegate is retired and must be absent from a rebuilt registry"
    );
    assert!(
        !names.contains(&"spawn_subagent".to_string()),
        "spawn_subagent is retired and must be absent from a rebuilt registry"
    );
    // The V1 entrypoint is the surviving spawn surface in the same
    // rebuilt registry (and refuses at depth > 0 per D1 — asserted
    // behaviorally by `subagent_v1::tests`).
    assert!(
        names.contains(&"reasoning_subagent".to_string()),
        "reasoning_subagent must be the surviving spawn surface in a rebuilt registry"
    );
}

#[test]
fn zigzag_is_counted_by_one_ledger_across_spawn_hops() {
    // SA-9/SA-10: any spawn chain (the census chain was
    // `delegate → spawn_subagent → delegate`; both legacy hops are
    // retired) is counted by ONE counter. The depth a rebuilt
    // registry sees is exactly the spawning context's lineage
    // advanced by one, however many hops the chain took.
    use zeroclaw_api::subagent_v1::{LineageRef, ParentRunRef};

    let root = LineageRef::new_root(ParentRunRef::from_opaque("chain-root"));
    assert_eq!(root.depth(), 0);

    let after_first_hop = root.child();
    assert_eq!(after_first_hop.depth(), 1);

    let after_second_hop = after_first_hop.child();
    assert_eq!(after_second_hop.depth(), 2);

    // A rebuilt registry in the grandchild context carries the same
    // lineage — depth 2 against the cap (3), refusing at the NEXT
    // hop, never resetting (the ledger law is what the test pins):
    let after_third_hop = after_second_hop.child();
    assert_eq!(after_third_hop.depth(), 3);
    // ...and 3 >= cap is the refusal asserted behaviorally on the
    // surviving spawn surface in `subagent_v1::tests`.
    assert!(after_third_hop.depth() >= 3);

    // The ledger identity is the root run, shared across the whole
    // chain (SA-11: rebuilds inherit, roots are typed transitions).
    assert_eq!(root.root_ref(), after_third_hop.root_ref());
}

#[test]
fn all_tools_includes_read_skill_in_compact_mode() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;

    let tools = all_tools(
        Arc::new(cfg.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"read_skill"));
}

#[test]
fn all_tools_excludes_read_skill_in_full_mode() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Full;

    let tools = all_tools(
        Arc::new(cfg.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(!names.contains(&"read_skill"));
}

#[test]
fn retired_operator_tools_absent_from_every_assembly_path() {
    // Totality check for the operator/admin retirement: the retired
    // names must not appear in the assembled registry (`tools`) NOR in
    // the pre-policy `unfiltered_tool_arcs` (the vector skill builtin
    // elevation resolves targets against), whatever re-admits them:
    //
    // - absent `composition` (resolves as full),
    // - explicit `composition = "full"`,
    // - config sections explicitly enabling the retired tools.
    fn assembled(
        tmp: &TempDir,
        composition: Option<zeroclaw_config::composition::Composition>,
        enable_retired_sections: bool,
    ) -> (Vec<String>, Vec<String>) {
        let security = Arc::new(SecurityPolicy::default());
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(
                &MemoryConfig {
                    backend: "markdown".into(),
                    ..MemoryConfig::default()
                },
                tmp.path(),
                None,
            )
            .unwrap(),
        );
        let mut cfg = test_config(tmp);
        cfg.composition = composition;
        if enable_retired_sections {
            cfg.security_ops.enabled = true;
            cfg.backup.enabled = true;
            cfg.data_retention.enabled = true;
        }
        let result = all_tools(
            Arc::new(cfg.clone()),
            &security,
            &zeroclaw_config::schema::RiskProfileConfig::default(),
            "test-agent",
            mem,
            &BrowserConfig::default(),
            &zeroclaw_config::schema::HttpRequestConfig::default(),
            &zeroclaw_config::schema::WebFetchConfig::default(),
            tmp.path(),
            &HashMap::new(),
            None,
            &cfg,
            None,
        );
        (
            result.tools.iter().map(|t| t.name().to_string()).collect(),
            result
                .unfiltered_tool_arcs
                .iter()
                .map(|t| t.name().to_string())
                .collect(),
        )
    }

    for (label, composition, enable) in [
        ("absent composition", None, false),
        (
            "explicit full composition",
            Some(zeroclaw_config::composition::Composition::Full),
            false,
        ),
        ("enabled retired sections, absent composition", None, true),
    ] {
        let tmp = TempDir::new().unwrap();
        let (tools, arcs) = assembled(&tmp, composition, enable);
        for retired in RETIRED_OPERATOR_TOOL_NAMES {
            assert!(
                !tools.iter().any(|n| n == retired),
                "retired tool {retired} leaked into the registry under {label}: {tools:?}"
            );
            assert!(
                !arcs.iter().any(|n| n == retired),
                "retired tool {retired} leaked into the unfiltered arcs under {label}: {arcs:?}"
            );
        }
    }
}

#[test]
fn all_tools_registers_read_skill_for_compact_agent_override_over_global_full() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    // Global stays Full; a runtime profile flips this agent to Compact and
    // the agent selects it via `runtime_profile`.
    cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Full;
    cfg.runtime_profiles.insert(
        "compact_profile".to_string(),
        zeroclaw_config::schema::RuntimeProfileConfig {
            prompt_injection_mode: Some(
                zeroclaw_config::schema::SkillsPromptInjectionMode::Compact,
            ),
            ..Default::default()
        },
    );
    cfg.agents.insert(
        "test-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            runtime_profile: "compact_profile".into(),
            ..Default::default()
        },
    );

    let tools = all_tools(
        Arc::new(cfg.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(
        names.contains(&"read_skill"),
        "compact runtime-profile override should register read_skill even when global is full"
    );
}

#[test]
fn all_tools_omits_read_skill_for_full_agent_override_over_global_compact() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let browser = BrowserConfig::default();
    let http = zeroclaw_config::schema::HttpRequestConfig::default();
    let mut cfg = test_config(&tmp);
    // Global is Compact; a runtime profile pins this agent to Full and the
    // agent selects it via `runtime_profile`.
    cfg.skills.prompt_injection_mode = zeroclaw_config::schema::SkillsPromptInjectionMode::Compact;
    cfg.runtime_profiles.insert(
        "full_profile".to_string(),
        zeroclaw_config::schema::RuntimeProfileConfig {
            prompt_injection_mode: Some(zeroclaw_config::schema::SkillsPromptInjectionMode::Full),
            ..Default::default()
        },
    );
    cfg.agents.insert(
        "test-agent".to_string(),
        zeroclaw_config::schema::AliasedAgentConfig {
            runtime_profile: "full_profile".into(),
            ..Default::default()
        },
    );

    let tools = all_tools(
        Arc::new(cfg.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &browser,
        &http,
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();
    assert!(
        !names.contains(&"read_skill"),
        "full runtime-profile override should omit read_skill even when global is compact"
    );
}

/// `vi_verify` checked caller-supplied constraints against a caller-supplied
/// fulfillment with nothing establishing that either came from a signed
/// credential. Until a chain verifier exists the tool must not reach the model
/// even when an operator opts in.
#[test]
fn vi_verify_is_not_registered_even_when_verifiable_intent_is_enabled() {
    let tmp = TempDir::new().unwrap();
    let security = Arc::new(SecurityPolicy::default());
    let mem_cfg = MemoryConfig {
        backend: "markdown".into(),
        ..MemoryConfig::default()
    };
    let mem: Arc<dyn Memory> =
        Arc::from(zeroclaw_memory::create_memory(&mem_cfg, tmp.path(), None).unwrap());

    let mut cfg = test_config(&tmp);
    cfg.verifiable_intent.enabled = true;

    let tools = all_tools(
        Arc::new(cfg.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "test-agent",
        mem,
        &BrowserConfig::default(),
        &zeroclaw_config::schema::HttpRequestConfig::default(),
        &zeroclaw_config::schema::WebFetchConfig::default(),
        tmp.path(),
        &HashMap::new(),
        None,
        &cfg,
        None,
    )
    .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name()).collect();

    assert!(
        !names.contains(&"vi_verify"),
        "vi_verify must not be model-callable while no chain verifier exists"
    );
    assert!(
        names.contains(&"shell"),
        "positive control: the registry must still be populated"
    );
}

#[cfg(test)]
mod wrapper_spec_forwarding_tests {
    use super::*;
    use async_trait::async_trait;
    use zeroclaw_api::tool::ToolSpec;

    /// Stand-in for `McpToolWrapper`: stores its schema once and overrides
    /// `spec()` to hand out `Arc::clone`, so tests can assert wrappers
    /// preserve `Arc` identity instead of falling back to the trait
    /// default (which would deep-clone via `parameters_schema()`).
    struct ArcSchemaTool {
        schema: Arc<serde_json::Value>,
    }

    impl ArcSchemaTool {
        fn new() -> Self {
            Self {
                schema: Arc::new(serde_json::json!({
                    "type": "object",
                    "properties": { "path": { "type": "string" } }
                })),
            }
        }
    }

    impl ::zeroclaw_api::attribution::Attributable for ArcSchemaTool {
        fn role(&self) -> ::zeroclaw_api::attribution::Role {
            ::zeroclaw_api::attribution::Role::Tool(::zeroclaw_api::attribution::ToolKind::Plugin)
        }
        fn alias(&self) -> &str {
            "arc-schema-tool"
        }
    }

    #[async_trait]
    impl Tool for ArcSchemaTool {
        fn name(&self) -> &str {
            "arc_schema_tool"
        }

        fn description(&self) -> &str {
            "test tool with Arc-shared schema"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            (*self.schema).clone()
        }

        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: self.name().to_string(),
                description: self.description().to_string(),
                parameters: Arc::clone(&self.schema),
                output: None,
                param_domains: std::collections::BTreeMap::new(),
            }
        }

        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: "ok".into(),
                error: None,
            })
        }
    }

    #[test]
    fn arc_tool_ref_forwards_spec_arc_identity() {
        let inner: Arc<dyn Tool> = Arc::new(ArcSchemaTool::new());
        let inner_params = inner.spec().parameters;
        let wrapped = ArcToolRef(Arc::clone(&inner));

        assert!(
            Arc::ptr_eq(&wrapped.spec().parameters, &inner_params),
            "ArcToolRef must forward spec() so the inner Arc-shared schema \
             survives; the trait default deep-clones it every call"
        );
        assert!(
            Arc::ptr_eq(&wrapped.spec().parameters, &wrapped.spec().parameters),
            "repeated spec() calls must hand out the same allocation"
        );
    }

    #[test]
    fn arc_delegating_tool_forwards_spec_arc_identity() {
        let inner: Arc<dyn Tool> = Arc::new(ArcSchemaTool::new());
        let inner_params = inner.spec().parameters;
        let boxed = ArcDelegatingTool::boxed(inner);

        assert!(
            Arc::ptr_eq(&boxed.spec().parameters, &inner_params),
            "ArcDelegatingTool must forward spec() so the inner Arc-shared \
             schema survives; the trait default deep-clones it every call"
        );
    }
}

#[tokio::test]
async fn tone_proposals_resolve_defaults_in_fixed_and_live_registries() {
    use zeroclaw_config::persona::{PersonaKnobs, PersonaLevel};
    for use_live in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut cfg = lineage_registry_config();
        cfg.data_dir = tmp.path().join("data");
        std::fs::create_dir(&cfg.data_dir).unwrap();
        cfg.personas.insert(
            "reserved".into(),
            PersonaKnobs {
                humor: PersonaLevel::Low,
                ..Default::default()
            },
        );
        cfg.agents.get_mut("parent-agent").unwrap().persona = "reserved".into();
        let live = Arc::new(parking_lot::RwLock::new(cfg.clone()));
        let mem: Arc<dyn Memory> = Arc::from(
            zeroclaw_memory::create_memory(
                &MemoryConfig {
                    backend: "markdown".into(),
                    ..Default::default()
                },
                tmp.path(),
                None,
            )
            .unwrap(),
        );
        let security = Arc::new(SecurityPolicy {
            workspace_dir: tmp.path().to_path_buf(),
            ..Default::default()
        });
        let tools = all_tools_with_runtime(
            Arc::new(cfg.clone()),
            &security,
            &cfg.risk_profiles["default"],
            "parent-agent",
            Arc::new(NativeRuntime::new()),
            mem,
            &BrowserConfig::default(),
            &Default::default(),
            &Default::default(),
            tmp.path(),
            &cfg.agents,
            None,
            &cfg,
            None,
            use_live.then(|| live.clone()),
            None,
            None,
        );
        // Mutation after registry construction must affect the live branch,
        // while the fixed request retains its canonical immutable config.
        live.write().personas.get_mut("reserved").unwrap().humor = PersonaLevel::Xhigh;
        let tool = tools
            .tools
            .iter()
            .find(|tool| tool.name() == "propose_soul_change")
            .unwrap();
        let result = tool
            .execute(serde_json::json!({
                "layer":"tone", "trait_key":"humor", "level":"medium",
                "proposal":"Use occasional humour", "rationale":"Owner requested it"
            }))
            .await
            .unwrap();
        assert_eq!(result.success, !use_live, "{:?}", result.error);
    }
}
