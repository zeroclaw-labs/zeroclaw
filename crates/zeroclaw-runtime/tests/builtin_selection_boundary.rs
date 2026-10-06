use std::collections::BTreeSet;
use std::sync::Arc;

use tempfile::TempDir;
use zeroclaw_api::runtime_traits::RuntimeAdapter;
use zeroclaw_config::builtin_tools::CORE_TOOL_NAMES;
use zeroclaw_config::platform::NativeRuntime;
use zeroclaw_config::policy::SecurityPolicy;
use zeroclaw_config::schema::{AliasedAgentConfig, Config, RiskProfileConfig};
use zeroclaw_memory::{Memory, NoneMemory};
use zeroclaw_runtime::tools::scoped::{ScopedAssembled, ScopedAssembly, ScopedToolRegistry};

fn fixture(tmp: &TempDir) -> Config {
    let workspace = tmp.path().join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.memory.backend = "none".into();
    config.plugins.enabled = false;
    config
        .agents
        .insert("boundary".into(), AliasedAgentConfig::default());
    config
        .providers
        .models
        .openai
        .insert("boundary".into(), Default::default());
    config.agents.get_mut("boundary").unwrap().model_provider = "openai.boundary".into();
    config.knowledge.db_path = tmp
        .path()
        .join("optional-knowledge.db")
        .display()
        .to_string();
    config
}

async fn assemble(
    tmp: &TempDir,
    config: &Config,
    allowed: Option<Vec<String>>,
    excluded: Option<Vec<String>>,
    caller_allowed: Option<&[String]>,
) -> ScopedAssembled {
    assemble_context(tmp, config, allowed, excluded, caller_allowed, false, false).await
}

async fn assemble_context(
    tmp: &TempDir,
    config: &Config,
    allowed: Option<Vec<String>>,
    excluded: Option<Vec<String>>,
    caller_allowed: Option<&[String]>,
    exclude_memory: bool,
    acp_delivery: bool,
) -> ScopedAssembled {
    let workspace = tmp.path().join("workspace");
    let security = Arc::new(SecurityPolicy {
        workspace_dir: workspace.clone(),
        allowed_tools: allowed,
        excluded_tools: excluded,
        ..SecurityPolicy::default()
    });
    let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
    let memory: Arc<dyn Memory> = Arc::new(NoneMemory::new("boundary"));
    let built = zeroclaw_runtime::tools::all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        &RiskProfileConfig::default(),
        "boundary",
        Arc::clone(&runtime),
        memory,
        None,
        None,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        &workspace,
        &config.agents,
        None,
        config,
        None,
        false,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    ScopedToolRegistry::assemble(ScopedAssembly {
        config,
        agent_alias: "boundary",
        security: &security,
        built,
        skills: &[],
        runtime,
        caller_allowed,
        connect_mcp: false,
        connect_peripherals: false,
        exclude_memory,
        acp_delivery,
        list_deferred_mcp_specs: false,
        emit_assembly_logs: false,
        mcp_registry: None,
    })
    .await
}

fn names(assembled: &ScopedAssembled) -> BTreeSet<String> {
    assembled
        .registry
        .iter()
        .map(|tool| tool.spec().name)
        .collect()
}

#[tokio::test]
async fn default_model_catalog_is_eleven_before_optional_store_preparation() {
    let tmp = TempDir::new().unwrap();
    let mut config = fixture(&tmp);
    config.knowledge.enabled = true;
    let assembled = assemble(&tmp, &config, None, None, None).await;

    assert!(
        !tmp.path().join("optional-knowledge.db").exists(),
        "unselected knowledge preparation must not open a store"
    );
    assert!(
        !config.data_dir.exists(),
        "unselected session tools must not open their shared store"
    );
    assert_eq!(
        names(&assembled),
        CORE_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect()
    );
    assert!(assembled.delegate_handle.is_none());
    assert!(assembled.ask_user_handle.is_none());
    assert!(assembled.poll_handle.is_none());
}

#[tokio::test]
async fn selected_calculator_executes_through_the_scoped_registry() {
    let tmp = TempDir::new().unwrap();
    let mut config = fixture(&tmp);
    config.tools.optional = vec!["calculator".into()];
    let assembled = assemble(&tmp, &config, None, None, None).await;
    let mut expected: BTreeSet<String> = CORE_TOOL_NAMES
        .iter()
        .map(|name| (*name).to_string())
        .collect();
    expected.insert("calculator".into());
    assert_eq!(names(&assembled), expected);
    let calculator = assembled
        .registry
        .iter()
        .find(|tool| tool.name() == "calculator")
        .unwrap();
    let result = calculator
        .execute(serde_json::json!({"function": "multiply", "values": [6, 7]}))
        .await
        .unwrap();
    assert!(result.success, "{result:?}");
    assert!(result.output.contains("42"), "{result:?}");
}

#[tokio::test]
async fn selection_never_expands_agent_or_caller_permission() {
    let tmp = TempDir::new().unwrap();
    let mut config = fixture(&tmp);
    config.tools.optional = vec!["calculator".into()];
    let shell_only = vec!["shell".to_string()];

    let agent_denied = assemble(&tmp, &config, Some(shell_only.clone()), None, None).await;
    assert_eq!(names(&agent_denied), BTreeSet::from(["shell".to_string()]));

    let caller_denied = assemble(&tmp, &config, None, None, Some(&shell_only)).await;
    assert_eq!(names(&caller_denied), BTreeSet::from(["shell".to_string()]));

    let excluded = assemble(&tmp, &config, None, Some(vec!["calculator".into()]), None).await;
    assert!(!names(&excluded).contains("calculator"));
    assert!(names(&excluded).contains("git_operations"));
}

#[tokio::test]
async fn selected_knowledge_still_requires_its_canonical_enabled_setting() {
    let tmp = TempDir::new().unwrap();
    let mut config = fixture(&tmp);
    config.tools.optional = vec!["knowledge".into()];
    config.knowledge.enabled = false;
    let inactive = assemble(&tmp, &config, None, None, None).await;
    assert!(!names(&inactive).contains("knowledge"));
    assert!(!tmp.path().join("optional-knowledge.db").exists());

    config.knowledge.enabled = true;
    let active = assemble(&tmp, &config, None, None, None).await;
    assert!(tmp.path().join("optional-knowledge.db").is_file());
    let knowledge = active
        .registry
        .iter()
        .find(|tool| tool.name() == "knowledge")
        .unwrap();
    let result = knowledge
        .execute(serde_json::json!({"action": "graph_stats"}))
        .await
        .unwrap();
    assert!(result.success, "{result:?}");
}

// Exact historical Chat fixture from accepted 080b/d6074ef1 (no configured
// optional integrations); the five native adapters omitted there are restored
// only when tools-external is compiled. This is a fixture, not a universal count.
fn full_chat_fixture_names() -> BTreeSet<String> {
    let mut names: BTreeSet<_> = [
        "TodoWrite",
        "ask_user",
        "backup",
        "calculator",
        "canvas",
        "channel_room",
        "content_search",
        "cron_add",
        "cron_list",
        "cron_remove",
        "cron_run",
        "cron_runs",
        "cron_update",
        "delegate",
        "escalate_to_human",
        "file_edit",
        "file_read",
        "file_write",
        "git_forge",
        "git_operations",
        "glob_search",
        "http_request",
        "image_info",
        "llm_task",
        "memory_export",
        "memory_forget",
        "memory_purge",
        "memory_recall",
        "memory_store",
        "model_routing_config",
        "model_switch",
        "poll",
        "proxy_config",
        "reaction",
        "schedule",
        "send_message_to_peer",
        "send_via",
        "sessions_current",
        "sessions_history",
        "sessions_list",
        "sessions_send",
        "shell",
        "spawn_subagent",
        "web_fetch",
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    if cfg!(feature = "tools-external") {
        names.extend(
            [
                "browser_open",
                "pushover",
                "screenshot",
                "weather",
                "web_search_tool",
            ]
            .map(str::to_string),
        );
    }
    names
}

#[tokio::test]
async fn minimal_acp_catalog_is_eight_without_optional_preparation() {
    let tmp = TempDir::new().unwrap();
    let config = fixture(&tmp);
    let assembled = assemble_context(&tmp, &config, None, None, None, true, true).await;
    let expected = CORE_TOOL_NAMES
        .iter()
        .filter(|name| !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(name))
        .map(|name| (*name).to_string())
        .collect();
    assert_eq!(names(&assembled), expected);
    assert_eq!(assembled.registry.len(), 8);
    assert!(!config.data_dir.exists());
}

#[tokio::test]
async fn full_chat_and_acp_restore_the_matching_builtin_fixture() {
    for acp in [false, true] {
        let tmp = TempDir::new().unwrap();
        let mut config = fixture(&tmp);
        config.tools.optional = vec!["*".into()];
        let assembled = assemble_context(&tmp, &config, None, None, None, acp, acp).await;
        let mut expected = full_chat_fixture_names();
        if acp {
            expected.retain(|name| !zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&name.as_str()));
            expected.insert("deliver_file".into());
        }
        assert_eq!(names(&assembled), expected);
        assert!(assembled.delegate_handle.is_some());
        assert!(assembled.ask_user_handle.is_some());
        assert!(!tmp.path().join("optional-knowledge.db").exists());
        for tool in zeroclaw_config::opt_in_tools::OptInTool::ALL {
            assert!(
                !names(&assembled).contains(tool.section()),
                "full must not enable {}",
                tool.section()
            );
        }
        let calculator = assembled
            .registry
            .iter()
            .find(|tool| tool.name() == "calculator")
            .unwrap();
        let result = calculator
            .execute(serde_json::json!({"function":"multiply","values":[6,7]}))
            .await
            .unwrap();
        assert!(result.success && result.output.contains("42"), "{result:?}");
        println!(
            "full fixture ACP={acp}: {}",
            serde_json::to_string(&names(&assembled)).unwrap()
        );
    }
}

#[tokio::test]
async fn full_wildcard_with_names_does_not_expand_permission() {
    let tmp = TempDir::new().unwrap();
    let mut config = fixture(&tmp);
    config.tools.optional = vec!["*".into(), "calculator".into(), "weather".into()];
    let shell_only = vec!["shell".to_string()];
    let agent = assemble(&tmp, &config, Some(shell_only.clone()), None, None).await;
    assert_eq!(names(&agent), BTreeSet::from(["shell".into()]));
    let caller = assemble(&tmp, &config, None, None, Some(&shell_only)).await;
    assert_eq!(names(&caller), BTreeSet::from(["shell".into()]));
    let excluded = assemble(&tmp, &config, None, Some(vec!["calculator".into()]), None).await;
    let mut expected = full_chat_fixture_names();
    expected.remove("calculator");
    assert_eq!(names(&excluded), expected);
}
