//! Real Wasm namespace controls on the exact c1 factory/scoped seam.
use super::*;
use crate::tools::scoped::{ScopedAssembled, ScopedAssembly, ScopedToolRegistry};
use tempfile::TempDir;

type BuiltCase = (
    TempDir,
    Config,
    Arc<SecurityPolicy>,
    AllToolsResult,
    Arc<dyn RuntimeAdapter>,
);

fn build_case(optional: &[&str], pipeline_enabled: bool, plugins_enabled: bool) -> BuiltCase {
    let tmp = TempDir::new().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.tools.optional = optional.iter().map(|name| (*name).to_string()).collect();
    config.memory.backend = "none".into();
    config.pipeline.enabled = pipeline_enabled;
    config.knowledge.enabled = true;
    config.knowledge.db_path = tmp
        .path()
        .join("unselected-knowledge.db")
        .display()
        .to_string();
    config
        .agents
        .insert("namespace".into(), AliasedAgentConfig::default());
    config
        .providers
        .models
        .openai
        .insert("namespace".into(), Default::default());
    config.agents.get_mut("namespace").unwrap().model_provider = "openai.namespace".into();
    config.plugins.enabled = plugins_enabled;
    config.plugins.auto_discover = true;
    config.plugins.plugins_dir = tmp.path().join("plugins").display().to_string();
    let package = config
        .plugins
        .resolved_plugins_dir()
        .join("namespace-probe");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(
        package.join("manifest.toml"),
        "name = \"namespace-probe\"\nversion = \"0.1.0\"\nwasm_path = \"probe.wasm\"\ncapabilities = [\"tool\"]\n",
    )
    .unwrap();
    std::fs::write(
        package.join("probe.wasm"),
        include_bytes!("../../tests/fixtures/r5-namespace/namespace.component"),
    )
    .unwrap();
    let security = Arc::new(SecurityPolicy {
        workspace_dir: tmp.path().into(),
        ..SecurityPolicy::default()
    });
    let runtime: Arc<dyn RuntimeAdapter> = Arc::new(NativeRuntime::new());
    let memory: Arc<dyn Memory> = Arc::new(zeroclaw_memory::NoneMemory::new("namespace"));
    let built = all_tools_with_runtime(
        Arc::new(config.clone()),
        &security,
        &zeroclaw_config::schema::RiskProfileConfig::default(),
        "namespace",
        Arc::clone(&runtime),
        memory,
        None,
        None,
        &config.browser,
        &config.http_request,
        &config.web_fetch,
        tmp.path(),
        &config.agents,
        None,
        &config,
        None,
        false,
        None,
        None,
        None,
        None,
    )
    .unwrap();
    (tmp, config, security, built, runtime)
}

fn actual_guest(config: &Config) -> Arc<dyn Tool> {
    let host = Arc::new(
        zeroclaw_plugins::host::PluginHost::from_plugins_dir(
            &config.plugins.resolved_plugins_dir(),
        )
        .unwrap(),
    );
    let details = host.tool_plugin_details();
    let (manifest, component) = details.first().expect("actual fixture package admitted");
    let scope = zeroclaw_plugins::instance::PluginInstanceScope::for_package_binding(
        manifest,
        zeroclaw_plugins::PluginCapability::Tool,
        manifest.permissions.iter().copied(),
    )
    .unwrap();
    let services = plugin_host_services(Arc::clone(&host), Arc::new(config.clone()), None);
    let guest = zeroclaw_plugins::wasm_tool::WasmTool::from_wasm(
        (*component).clone(),
        scope,
        services,
        zeroclaw_plugins::component::PluginLimits {
            call_fuel: 10_000_000,
            max_memory_bytes: 64 * 1024 * 1024,
            max_table_elements: 10_000,
            max_instances: 128,
            call_timeout: std::time::Duration::from_secs(30),
        },
        None,
    )
    .expect("real guest metadata probe");
    assert_eq!(guest.name(), PipelineTool::NAME);
    Arc::new(guest)
}

async fn assemble_case(
    config: &Config,
    security: &Arc<SecurityPolicy>,
    built: AllToolsResult,
    runtime: Arc<dyn RuntimeAdapter>,
    skills: &[crate::skills::Skill],
    caller_allowed: Option<&[String]>,
) -> ScopedAssembled {
    ScopedToolRegistry::assemble(ScopedAssembly {
        config,
        agent_alias: "namespace",
        security,
        built,
        skills,
        runtime,
        caller_allowed,
        connect_mcp: false,
        connect_peripherals: false,
        exclude_memory: false,
        acp_delivery: false,
        list_deferred_mcp_specs: false,
        emit_assembly_logs: false,
        mcp_registry: None,
    })
    .await
}

fn guest_role(tool: &dyn Tool) -> bool {
    matches!(
        tool.role(),
        zeroclaw_api::attribution::Role::Tool(zeroclaw_api::attribution::ToolKind::WasmPlugin)
    )
}

#[tokio::test]
async fn r5_names_factory_refuses_real_guest_claiming_enabled_unselected_pipeline() {
    let (_tmp, _config, _security, built, _runtime) = build_case(&[], true, true);
    if let Some(guest) = built
        .tools
        .iter()
        .find(|tool| tool.name() == PipelineTool::NAME)
    {
        assert!(guest_role(guest.as_ref()));
        let executed = guest.execute(serde_json::json!({})).await.unwrap();
        assert!(executed.success);
        assert_eq!(executed.output.as_str(), "synthetic guest executed");
        eprintln!("R5_REAL_GUEST_BEFORE: execute_pipeline admitted and executed");
    }
    assert!(
        built
            .tools
            .iter()
            .all(|tool| tool.name() != PipelineTool::NAME),
        "configured but unselected host namespace was claimed by an actual Wasm guest"
    );
}

#[tokio::test]
async fn r5_names_scoped_refuses_real_guest_in_registry_parent_and_elevation_views() {
    let (_tmp, config, security, mut built, runtime) = build_case(&[], true, true);
    built.tools.retain(|tool| !guest_role(tool.as_ref()));
    built
        .unfiltered_tool_arcs
        .retain(|tool| !guest_role(tool.as_ref()));
    let guest = actual_guest(&config);
    let result = guest.execute(serde_json::json!({})).await.unwrap();
    assert_eq!(result.output.as_str(), "synthetic guest executed");
    built.tools.push(Box::new(ArcToolRef(Arc::clone(&guest))));
    built.unfiltered_tool_arcs.push(Arc::clone(&guest));
    let parent = Arc::new(RwLock::new(vec![guest]));
    built.delegate_handle = Some(Arc::clone(&parent));
    let skill = crate::skills::Skill {
        name: "namespace-skill".into(),
        description: "Namespace boundary fixture".into(),
        description_localizations: Default::default(),
        version: "0.1.0".into(),
        author: None,
        tags: Vec::new(),
        tools: vec![crate::skills::SkillTool {
            name: "invoke".into(),
            description: "Invoke the existing pipeline target".into(),
            kind: "builtin".into(),
            command: String::new(),
            args: Default::default(),
            target: Some(PipelineTool::NAME.to_string()),
            locked_args: Default::default(),
            timeout_secs: None,
        }],
        prompts: Vec::new(),
        slash_options: Vec::new(),
        always: false,
        location: None,
    };
    let assembled = assemble_case(&config, &security, built, runtime, &[skill], None).await;
    let wrapper = skill_tool::composed_tool_name("namespace-skill", "invoke");
    let registry_claim = assembled
        .registry
        .iter()
        .any(|tool| tool.name() == PipelineTool::NAME);
    let parent_claim = parent
        .read()
        .iter()
        .any(|tool| tool.name() == PipelineTool::NAME);
    let elevation_claim = assembled.registry.iter().any(|tool| tool.name() == wrapper);
    eprintln!(
        "R5_NAMESPACE_VIEWS: registry={registry_claim} parent={parent_claim} elevation={elevation_claim}"
    );
    assert!(!registry_claim && !parent_claim && !elevation_claim);
}

#[tokio::test]
async fn r5_names_disabled_host_allows_real_nonconflicting_guest_execution() {
    let (_tmp, config, security, built, runtime) = build_case(&[], false, true);
    assert!(
        built
            .tools
            .iter()
            .any(|tool| tool.name() == PipelineTool::NAME)
    );
    let assembled = assemble_case(&config, &security, built, runtime, &[], None).await;
    let guest = crate::agent::tool_execution::find_tool(&assembled.registry, PipelineTool::NAME)
        .expect("disabled host leaves the guest namespace available");
    assert!(guest_role(guest));
    let result = guest.execute(serde_json::json!({})).await.unwrap();
    assert!(result.success);
    assert_eq!(result.output.as_str(), "synthetic guest executed");
}

#[tokio::test]
async fn r5_names_caller_ceiling_still_refuses_an_admitted_real_guest() {
    let (_tmp, config, security, built, runtime) = build_case(&[], false, true);
    assert!(
        built
            .tools
            .iter()
            .any(|tool| tool.name() == PipelineTool::NAME)
    );
    let ceiling = vec!["shell".to_string()];
    let assembled = assemble_case(&config, &security, built, runtime, &[], Some(&ceiling)).await;
    assert!(
        crate::agent::tool_execution::find_tool(&assembled.registry, PipelineTool::NAME).is_none()
    );
    assert!(assembled.registry.iter().all(|tool| tool.name() == "shell"));
}

#[tokio::test]
async fn r5_names_minimal_eleven_keeps_optional_preparation_and_handles_absent() {
    let (tmp, config, security, built, runtime) = build_case(&[], true, false);
    assert!(built.delegate_handle.is_none());
    assert!(built.ask_user_handle.is_none());
    assert!(built.poll_handle.is_none());
    assert!(!tmp.path().join("unselected-knowledge.db").exists());
    assert!(!config.data_dir.exists());
    let assembled = assemble_case(&config, &security, built, runtime, &[], None).await;
    let observed: std::collections::BTreeSet<_> = assembled
        .registry
        .iter()
        .map(|tool| tool.name().to_string())
        .collect();
    let expected: std::collections::BTreeSet<String> =
        zeroclaw_config::builtin_tools::CORE_TOOL_NAMES
            .iter()
            .map(|name| (*name).to_string())
            .collect();
    assert_eq!(observed, expected);
}

#[tokio::test]
async fn r5_names_selected_native_pipeline_preserves_policy_refusal() {
    let (_tmp, mut config, _security, built, runtime) =
        build_case(&["execute_pipeline"], true, false);
    config.pipeline.allowed_tools = vec!["file_read".into(), "file_write".into()];
    let security = Arc::new(SecurityPolicy {
        allowed_tools: Some(vec![PipelineTool::NAME.into(), "file_read".into()]),
        ..SecurityPolicy::default()
    });
    let ceiling = vec![PipelineTool::NAME.to_string(), "file_read".to_string()];
    let assembled = assemble_case(&config, &security, built, runtime, &[], Some(&ceiling)).await;
    let pipeline = crate::agent::tool_execution::find_tool(&assembled.registry, PipelineTool::NAME)
        .expect("selected native pipeline is present");
    assert!(!guest_role(pipeline));
    let result = pipeline
        .execute(serde_json::json!({
            "steps": [{"tool": "file_write", "args": {"path": "forbidden", "content": "no"}}]
        }))
        .await
        .unwrap();
    assert!(!result.success);
    assert!(
        assembled
            .registry
            .iter()
            .all(|tool| tool.name() != "file_write")
    );
}
