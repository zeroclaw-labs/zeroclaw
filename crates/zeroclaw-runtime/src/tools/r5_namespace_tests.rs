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

// Late native ownership: genuine WIT metadata and granted HTTP MCP, with no
// replacement guest, registry, constructor, or principal shortcut.
use super::late_tool_search_observer as late_observer;
use futures_util::FutureExt as _;
use serde_json::{Value as LateValue, json as late_json};
use sha2::Digest as _;

fn late_digest(bytes: &[u8]) -> String {
    format!("{:x}", sha2::Sha256::digest(bytes))
}

fn late_owned_read(path: &std::path::Path, limit: u64) -> Vec<u8> {
    use std::io::Read as _;
    let meta = std::fs::symlink_metadata(path).expect("owned proof input metadata");
    assert!(meta.is_file() && !meta.file_type().is_symlink());
    assert!(meta.len() > 0 && meta.len() <= limit);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid();
        assert_eq!(meta.uid(), owner);
        assert_eq!(
            meta.mode() & 0o022,
            0,
            "proof input must not be writable by others"
        );
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .unwrap()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes.len() as u64, meta.len());
    bytes
}

fn late_inputs() -> (Vec<u8>, Vec<u8>, LateValue) {
    let receipt_path = std::path::PathBuf::from(
        std::env::var_os("ZEROCLAW_LATE_TOOL_SEARCH_FIXTURE_RECEIPT")
            .expect("explicit fixture receipt required"),
    );
    let receipt: LateValue =
        serde_json::from_slice(&late_owned_read(&receipt_path, 32 * 1024)).unwrap();
    assert_eq!(receipt["schema"], 1);
    assert!(matches!(
        receipt["execution_kind"].as_str(),
        Some("github-actions" | "devbox")
    ));
    if receipt["execution_kind"] == "github-actions" {
        for (field, variable) in [
            ("run_id", "GITHUB_RUN_ID"),
            ("attempt", "GITHUB_RUN_ATTEMPT"),
            ("job_key", "GITHUB_JOB"),
            ("workflow_ref", "GITHUB_WORKFLOW_REF"),
            ("workflow_sha", "GITHUB_WORKFLOW_SHA"),
        ] {
            assert_eq!(
                receipt[field],
                std::env::var(variable).expect("current official execution binding")
            );
        }
    } else {
        assert!(
            receipt["execution_id"]
                .as_str()
                .is_some_and(|value| !value.is_empty() && value.len() <= 128)
        );
    }
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let expected_inputs = [
        "Cargo.toml",
        "Cargo.lock",
        "crates/zeroclaw-plugins/tests/fixtures/tool-fixture/Cargo.toml",
        "crates/zeroclaw-plugins/tests/fixtures/tool-fixture/src/lib.rs",
        "wit/v0/README.md",
        "wit/v0/channel.wit",
        "wit/v0/config.wit",
        "wit/v0/inbound.wit",
        "wit/v0/logging.wit",
        "wit/v0/memory.wit",
        "wit/v0/plugin-info.wit",
        "wit/v0/secrets.wit",
        "wit/v0/sockets.wit",
        "wit/v0/state.wit",
        "wit/v0/tool.wit",
        "wit/v0/types.wit",
        "wit/v0/websocket.wit",
    ];
    let inputs = receipt["inputs"].as_array().unwrap();
    assert_eq!(inputs.len(), expected_inputs.len());
    for path in expected_inputs {
        let matches: Vec<_> = inputs.iter().filter(|row| row["path"] == path).collect();
        assert_eq!(matches.len(), 1);
        let bytes = late_owned_read(&workspace.join(path), 4 * 1024 * 1024);
        assert_eq!(matches[0]["sha256"], late_digest(&bytes));
        assert_eq!(matches[0]["bytes"], bytes.len() as u64);
    }
    // These bind the running test executable to its actual local runtime source;
    // the external fixture producer is bound separately above.
    for (path, compiled) in [
        (
            "crates/zeroclaw-runtime/src/tools/mod.rs",
            include_bytes!("mod.rs").as_slice(),
        ),
        (
            "crates/zeroclaw-runtime/src/tools/scoped.rs",
            include_bytes!("scoped.rs").as_slice(),
        ),
        (
            "crates/zeroclaw-runtime/src/tools/skill_tool.rs",
            include_bytes!("skill_tool.rs").as_slice(),
        ),
        (
            "crates/zeroclaw-runtime/src/skills/mod.rs",
            include_bytes!("../skills/mod.rs").as_slice(),
        ),
        (
            "crates/zeroclaw-runtime/src/tools/r5_namespace_tests.rs",
            include_bytes!("r5_namespace_tests.rs").as_slice(),
        ),
    ] {
        assert_eq!(
            late_digest(&late_owned_read(&workspace.join(path), 4 * 1024 * 1024)),
            late_digest(compiled)
        );
    }
    let load = |kind: &str, env_name: &str| {
        let path = std::path::PathBuf::from(
            std::env::var_os(env_name).expect("explicit generated component required"),
        );
        assert!(path.is_absolute());
        let raw_meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(raw_meta.is_file() && !raw_meta.file_type().is_symlink());
        let path = path.canonicalize().unwrap();
        let bytes = late_owned_read(&path, 64 * 1024 * 1024);
        assert_eq!(receipt["components"][kind]["sha256"], late_digest(&bytes));
        assert_eq!(receipt["components"][kind]["bytes"], bytes.len() as u64);
        assert_eq!(receipt["components"][kind]["path"], path.to_str().unwrap());
        bytes
    };
    let default = load("default", "ZEROCLAW_LATE_TOOL_SEARCH_DEFAULT_COMPONENT");
    let namespace = load("namespace", "ZEROCLAW_LATE_TOOL_SEARCH_COMPONENT");
    assert_ne!(late_digest(&default), late_digest(&namespace));
    (default, namespace, receipt)
}

struct LateTask {
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    handle: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
struct LateRun {
    name: &'static str,
    trace: late_observer::CaseTrace,
    tasks: Arc<std::sync::Mutex<Vec<LateTask>>>,
}

fn late_executable() -> &'static LateValue {
    static BINARY: std::sync::OnceLock<LateValue> = std::sync::OnceLock::new();
    BINARY.get_or_init(|| {
        use std::io::Read as _;
        let path = std::env::current_exe().unwrap().canonicalize().unwrap();
        let mut file = std::fs::File::open(path).unwrap();
        let size = file.metadata().unwrap().len();
        assert!(size > 0 && size <= 2 * 1024 * 1024 * 1024);
        let mut hash = sha2::Sha256::new();
        let mut read = 0_u64;
        let mut chunk = [0_u8; 65536];
        loop {
            let count = file.read(&mut chunk).unwrap();
            if count == 0 {
                break;
            }
            read += count as u64;
            assert!(read <= size);
            hash.update(&chunk[..count]);
        }
        assert_eq!(read, size);
        late_json!({"bytes":size,"sha256":format!("{:x}",hash.finalize())})
    })
}

async fn late_run<F, Fut>(name: &'static str, body: F)
where
    F: FnOnce(LateRun) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let run = LateRun {
        name,
        trace: Default::default(),
        tasks: Default::default(),
    };
    let result = late_observer::scope_case(run.trace.clone(), async {
        // Heap-pin the nested case future before timeout and panic polling layers.
        let body_future = Box::pin(async { body(run.clone()).await });
        tokio::time::timeout(
            std::time::Duration::from_secs(45),
            std::panic::AssertUnwindSafe(body_future).catch_unwind(),
        )
        .await
    })
    .await;
    let tasks = std::mem::take(&mut *run.tasks.lock().unwrap());
    let mut cleanup_ok = true;
    for mut task in tasks {
        if let Some(stop) = task.stop.take() {
            let _ = stop.send(());
        }
        match tokio::time::timeout(std::time::Duration::from_secs(2), &mut task.handle).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => cleanup_ok = false,
            Err(_) => {
                task.handle.abort();
                cleanup_ok = false;
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task.handle).await;
            }
        }
    }
    let events = run.trace.events();
    let passed = matches!(&result, Ok(Ok(()))) && cleanup_ok;
    let receipt = late_json!({ "schema":1, "name":name, "passed":passed, "cleanup_ok":cleanup_ok,
        "events":events, "test_executable":late_executable(), "runtime_source_sha256":late_digest(include_bytes!("scoped.rs")), "fixture_receipt_sha256":late_digest(&late_owned_read(&std::path::PathBuf::from(std::env::var_os("ZEROCLAW_LATE_TOOL_SEARCH_FIXTURE_RECEIPT").unwrap()), 32768)) });
    let bytes = serde_json::to_vec(&receipt).unwrap();
    assert!(bytes.len() <= 512 * 1024);
    let dir = std::path::PathBuf::from(
        std::env::var_os("ZEROCLAW_LATE_TOOL_SEARCH_CASE_RECEIPTS")
            .expect("explicit private receipt directory"),
    );
    let meta = std::fs::symlink_metadata(&dir).unwrap();
    assert!(meta.is_dir() && !meta.file_type().is_symlink());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        assert_eq!(meta.mode() & 0o777, 0o700);
        assert_eq!(
            meta.uid(),
            std::fs::metadata(env!("CARGO_MANIFEST_DIR")).unwrap().uid()
        );
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    use std::io::Write as _;
    let mut file = options.open(dir.join(format!("{name}.json"))).unwrap();
    file.write_all(&bytes).unwrap();
    file.flush().unwrap();
    drop(file);
    eprintln!("LATE_CASE_RECEIPT {}", String::from_utf8(bytes).unwrap());
    assert!(cleanup_ok, "owned MCP fixture cleanup failed");
    match result {
        Ok(Ok(())) => {}
        Ok(Err(panic)) => std::panic::resume_unwind(panic),
        Err(_) => panic!("late ToolSearch cooperative case deadline expired"),
    }
}

#[derive(Clone, Default)]
struct LateLedger {
    rows: Arc<std::sync::Mutex<Vec<String>>>,
    failed: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
struct LateServerState {
    ledger: LateLedger,
    empty: bool,
}

struct LateServer {
    url: String,
    ledger: LateLedger,
}

impl LateServer {
    fn methods(&self) -> Vec<String> {
        self.ledger.rows.lock().unwrap().clone()
    }
    fn count(&self, method: &str) -> usize {
        self.methods()
            .iter()
            .filter(|value| *value == method)
            .count()
    }
    fn handshake(&self) {
        assert!(!self.ledger.failed.load(std::sync::atomic::Ordering::SeqCst));
        let rows = self.methods();
        assert!(rows.len() >= 3);
        assert_eq!(
            &rows[..3],
            ["initialize", "notifications/initialized", "tools/list"]
        );
        for method in ["initialize", "notifications/initialized", "tools/list"] {
            assert_eq!(self.count(method), 1);
        }
    }
}

async fn late_mcp_handler(
    axum::extract::State(state): axum::extract::State<LateServerState>,
    method_kind: axum::http::Method,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> axum::response::Response {
    use axum::response::IntoResponse as _;
    let refuse = || {
        state
            .ledger
            .failed
            .store(true, std::sync::atomic::Ordering::SeqCst);
        axum::http::StatusCode::BAD_REQUEST.into_response()
    };
    if method_kind != axum::http::Method::POST || body.len() > 16384 {
        return refuse();
    }
    let Ok(request) = serde_json::from_slice::<LateValue>(&body) else {
        return refuse();
    };
    let Some(method) = request["method"].as_str() else {
        return refuse();
    };
    if request["jsonrpc"] != "2.0" {
        return refuse();
    }
    {
        let mut rows = state.ledger.rows.lock().unwrap();
        if rows.len() >= 64 || rows.iter().map(String::len).sum::<usize>() + method.len() > 65536 {
            return refuse();
        }
        rows.push(method.to_owned());
    }
    if method != "initialize"
        && headers.get("mcp-session-id").and_then(|h| h.to_str().ok()) != Some("late-case-session")
    {
        return refuse();
    }
    if method == "notifications/initialized" {
        if request.get("id").is_some() {
            return refuse();
        }
        return axum::http::StatusCode::ACCEPTED.into_response();
    }
    if !request["id"].is_u64() {
        return refuse();
    }
    let result = match method {
        "initialize" => {
            late_json!({"protocolVersion":"2024-11-05", "capabilities":{"tools":{}}, "serverInfo":{"name":"late-namespace-fixture","version":"1"}})
        }
        "tools/list" => {
            let tools: Vec<_> = if state.empty {
                Vec::new()
            } else {
                ["allowed", "denied"].iter().map(|name| late_json!({"name":name,"description":format!("late fixture {name}"),"inputSchema":{"type":"object","properties":{"text":{"type":"string"}}}})).collect()
            };
            late_json!({"tools":tools})
        }
        "tools/call" => {
            if request["params"]["name"] != "allowed"
                || !request["params"]["arguments"]["text"].is_string()
            {
                return refuse();
            }
            late_json!({"content":[{"type":"text","text":format!("late-mcp:{}",request["params"]["arguments"]["text"].as_str().unwrap())}],"isError":false})
        }
        _ => return refuse(),
    };
    let response = late_json!({"jsonrpc":"2.0","id":request["id"].clone(),"result":result});
    if serde_json::to_vec(&response).unwrap().len() > 8192 {
        return refuse();
    }
    (
        [("Mcp-Session-Id", "late-case-session")],
        axum::Json(response),
    )
        .into_response()
}

async fn late_mcp_unknown(
    axum::extract::State(state): axum::extract::State<LateServerState>,
) -> axum::http::StatusCode {
    state
        .ledger
        .failed
        .store(true, std::sync::atomic::Ordering::SeqCst);
    axum::http::StatusCode::NOT_FOUND
}

async fn late_server(run: &LateRun, empty: bool) -> LateServer {
    let ledger = LateLedger::default();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/mcp", listener.local_addr().unwrap());
    let router = axum::Router::new()
        .route("/mcp", axum::routing::any(late_mcp_handler))
        .fallback(late_mcp_unknown)
        .layer(axum::extract::DefaultBodyLimit::max(16384))
        .with_state(LateServerState {
            ledger: ledger.clone(),
            empty,
        });
    let (stop, done) = tokio::sync::oneshot::channel();
    let handle = ::zeroclaw_spawn::spawn!(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = done.await;
            })
            .await
            .unwrap();
    });
    run.tasks.lock().unwrap().push(LateTask {
        stop: Some(stop),
        handle,
    });
    LateServer { url, ledger }
}

struct LateCase {
    tmp: TempDir,
    config: Config,
    security: Arc<SecurityPolicy>,
    built: Option<AllToolsResult>,
    runtime: Arc<dyn RuntimeAdapter>,
    guest: Arc<dyn Tool>,
}

async fn late_build(
    run: &LateRun,
    optional: &[&str],
    pipeline: bool,
    builtin: bool,
    default_guest: bool,
    customize: impl FnOnce(&mut Config, &mut SecurityPolicy),
) -> LateCase {
    let (default, namespace, _) = late_inputs();
    let component = if default_guest { default } else { namespace };
    let tmp = TempDir::new().unwrap();
    let mut config = Config {
        data_dir: tmp.path().join("data"),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.tools.optional = optional.iter().map(|name| (*name).to_owned()).collect();
    config.memory.backend = "none".into();
    config.memory.auto_save = false;
    config.pipeline.enabled = pipeline;
    config.pipeline.allowed_tools = vec!["tool_search".into(), "calculator".into()];
    config.knowledge.enabled = true;
    config.knowledge.db_path = tmp
        .path()
        .join("unselected-knowledge.db")
        .display()
        .to_string();
    config.agents.insert(
        "namespace".into(),
        AliasedAgentConfig {
            model_provider: "custom.namespace".into(),
            runtime_profile: "late-profile".into(),
            risk_profile: "late-risk".into(),
            ..Default::default()
        },
    );
    let provider = config
        .providers
        .models
        .ensure("custom", "namespace")
        .unwrap();
    provider.api_key = Some("test-key".into());
    provider.model = Some("test-model".into());
    provider.uri = Some("http://127.0.0.1:9".into());
    config
        .risk_profiles
        .insert("late-risk".into(), Default::default());
    config.runtime_profiles.insert(
        "late-profile".into(),
        zeroclaw_config::schema::RuntimeProfileConfig {
            deferred_builtin_tools: builtin,
            ..Default::default()
        },
    );
    config.plugins.enabled = true;
    config.plugins.auto_discover = true;
    config.plugins.plugins_dir = tmp.path().join("plugins").display().to_string();
    let package = config.plugins.resolved_plugins_dir().join("tool-fixture");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("fixture.wasm"), &component).unwrap();
    std::fs::write(package.join("manifest.toml"), format!("name = \"tool-fixture\"\nversion = \"0.0.0\"\nwasm_path = \"fixture.wasm\"\nwasm_sha256 = \"{}\"\ncapabilities = [\"tool\"]\n",late_digest(&component))).unwrap();
    let mut security = SecurityPolicy {
        workspace_dir: tmp.path().into(),
        excluded_tools: Some(vec!["late__denied".into()]),
        ..Default::default()
    };
    customize(&mut config, &mut security);
    let security = Arc::new(security);
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
    let name = if default_guest {
        "config-echo"
    } else {
        "tool_search"
    };
    let guests: Vec<_> = built
        .unfiltered_tool_arcs
        .iter()
        .filter(|tool| tool.name() == name && guest_role(tool.as_ref()))
        .cloned()
        .collect();
    assert_eq!(
        guests.len(),
        1,
        "real guest setup must admit exactly one actual adapter"
    );
    let guest = guests[0].clone();
    let result = guest
        .execute(late_json!({"text":"late-preflight"}))
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(
        result.output.as_str(),
        "label=unset|uppercase=false|max_len=0|keys=0|text=late-preflight"
    );
    eprintln!("LATE_SETUP_COMPLETE::{}::real-wit-{}", run.name, name);
    LateCase {
        tmp,
        config,
        security,
        built: Some(built),
        runtime,
        guest,
    }
}

fn late_grant(config: &mut Config, url: &str) {
    config.mcp.enabled = true;
    config.mcp.deferred_loading = true;
    config.mcp.servers = vec![zeroclaw_config::schema::McpServerConfig {
        name: "late".into(),
        transport: zeroclaw_config::schema::McpTransport::Http,
        url: Some(url.into()),
        ..Default::default()
    }];
    config.mcp_bundles.insert(
        "late-bundle".into(),
        zeroclaw_config::schema::McpBundleConfig {
            servers: vec!["late".into()],
            exclude: Vec::new(),
        },
    );
    config.agents.get_mut("namespace").unwrap().mcp_bundles = vec!["late-bundle".into()];
}

async fn late_assemble(
    case: &mut LateCase,
    skills: &[crate::skills::Skill],
    caller: Option<&[String]>,
    connect: bool,
    listing: bool,
    exclude_memory: bool,
    shared: Option<Arc<McpRegistry>>,
) -> ScopedAssembled {
    ScopedToolRegistry::assemble(ScopedAssembly {
        config: &case.config,
        agent_alias: "namespace",
        security: &case.security,
        built: case.built.take().unwrap(),
        skills,
        runtime: case.runtime.clone(),
        caller_allowed: caller,
        connect_mcp: connect,
        connect_peripherals: false,
        exclude_memory,
        acp_delivery: false,
        list_deferred_mcp_specs: listing,
        emit_assembly_logs: false,
        mcp_registry: shared,
    })
    .await
}

fn late_skill(skill_name: &str, name: &str, target: &str) -> crate::skills::Skill {
    crate::skills::Skill {
        name: skill_name.into(),
        description: "late namespace fixture".into(),
        description_localizations: Default::default(),
        version: "0.0.0".into(),
        author: None,
        tags: Vec::new(),
        tools: vec![crate::skills::SkillTool {
            name: name.into(),
            description: "late fixture alias".into(),
            kind: "builtin".into(),
            command: String::new(),
            args: Default::default(),
            target: Some(target.into()),
            locked_args: Default::default(),
            timeout_secs: None,
        }],
        prompts: Vec::new(),
        slash_options: Vec::new(),
        always: false,
        location: None,
    }
}

fn late_oracle(run: &LateRun, view: &str, condition: bool) {
    assert!(condition, "LATE_NAMESPACE_ORACLE::{}::{view}", run.name);
}

fn late_guest_absent(run: &LateRun, out: &ScopedAssembled) {
    let found = out
        .registry
        .iter()
        .filter(|tool| tool.name() == "tool_search")
        .collect::<Vec<_>>();
    late_oracle(
        run,
        "top-native-owner",
        found.len() == 1 && !guest_role(found[0].as_ref()),
    );
}

fn late_pipeline_request(child: &str) -> LateValue {
    let args = if child == "calculator" {
        late_json!({"function":"add","values":[1,2]})
    } else {
        late_json!({"text":"nested"})
    };
    late_json!({"steps":[{"tool":child,"args":args}],"result":"last"})
}

async fn late_pipeline_oracles(run: &LateRun, tool: &dyn Tool) {
    let legitimate = tool
        .execute(late_pipeline_request("calculator"))
        .await
        .unwrap();
    assert!(legitimate.success);
    assert_eq!(legitimate.output.as_str(), "3");
    let nested = tool
        .execute(late_pipeline_request("tool_search"))
        .await
        .unwrap();
    if nested.success {
        assert_eq!(
            nested.output.as_str(),
            "label=unset|uppercase=false|max_len=0|keys=0|text=nested"
        );
    }
    late_oracle(run, "nested-real-guest-refused", !nested.success);
}

async fn late_activate(run: &LateRun, out: &ScopedAssembled, server: &LateServer) -> Arc<dyn Tool> {
    let search = out
        .tool_search_handle
        .as_ref()
        .expect("real native creator required");
    let query = search
        .execute(late_json!({"query":"allowed"}))
        .await
        .unwrap();
    assert!(query.success);
    assert_eq!(server.count("tools/call"), 0);
    let selected = search
        .execute(late_json!({"query":"select:late__allowed"}))
        .await
        .unwrap();
    assert!(selected.success);
    assert_eq!(server.count("tools/call"), 0);
    let state = out.activated_handle.as_ref().unwrap();
    let tool = state
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .get("late__allowed")
        .unwrap();
    assert!(
        !state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_activated("late__denied")
    );
    let result = tool
        .execute(late_json!({"text":"actual-call"}))
        .await
        .unwrap();
    assert!(result.success);
    assert!(result.output.as_str().contains("late-mcp:actual-call"));
    assert_eq!(server.count("tools/call"), 1);
    server.handshake();
    late_observer::record("protocol-positive", run.name, 1, 0);
    tool
}

#[tokio::test]
async fn late_tool_search_real_guest_metadata_preflight() {
    late_run(
        "late_tool_search_real_guest_metadata_preflight",
        |run| async move {
            let default = late_build(&run, &[], false, false, true, |_, _| {}).await;
            let namespace = late_build(&run, &[], false, false, false, |_, _| {}).await;
            assert_eq!(default.guest.name(), "config-echo");
            assert_eq!(namespace.guest.name(), "tool_search");
            assert!(guest_role(default.guest.as_ref()) && guest_role(namespace.guest.as_ref()));
            assert_eq!(default.guest.description(), namespace.guest.description());
            assert_eq!(
                default.guest.parameters_schema(),
                namespace.guest.parameters_schema()
            );
            assert_eq!(run.trace.count("wasm-probe-enter", "tool-fixture"), 2);
            assert_eq!(run.trace.count("wasm-admitted", "tool-fixture"), 2);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_deferred_mcp_registry_owner() {
    late_run(
        "late_tool_search_deferred_mcp_registry_owner",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(&run, &[], false, false, false, |config, _| {
                late_grant(config, &server.url)
            })
            .await;
            let out = late_assemble(&mut case, &[], None, true, false, false, None).await;
            server.handshake();
            assert!(out.tool_search_handle.is_some());
            late_guest_absent(&run, &out);
            assert_eq!(run.trace.count("constructor-return", "mcp-search"), 1);
            assert_eq!(run.trace.count("constructor-return", "builtin-search"), 0);
            assert_eq!(run.trace.count("mcp-connect", "actual"), 1);
            assert_eq!(run.trace.count("wasm-probe-enter", "tool-fixture"), 1);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_deferred_mcp_delegate_retirement() {
    late_run(
        "late_tool_search_deferred_mcp_delegate_retirement",
        |run| async move {
            // The natural factory snapshot precedes plugin append, on both sources.
            let server = late_server(&run, false).await;
            let mut natural = late_build(&run, &["delegate"], false, false, false, |config, _| {
                late_grant(config, &server.url)
            })
            .await;
            let delegate = natural
                .built
                .as_ref()
                .unwrap()
                .delegate_tool
                .as_ref()
                .unwrap()
                .clone();
            let parent = natural
                .built
                .as_ref()
                .unwrap()
                .delegate_handle
                .as_ref()
                .unwrap()
                .clone();
            assert!(Arc::ptr_eq(&parent, &delegate.parent_tools_handle()));
            assert!(
                !parent
                    .read()
                    .iter()
                    .any(|tool| guest_role(tool.as_ref()) && tool.name() == "tool_search")
            );
            let out = late_assemble(&mut natural, &[], None, true, false, false, None).await;
            server.handshake();
            let activated = late_activate(&run, &out, &server).await;
            assert!(
                parent
                    .read()
                    .iter()
                    .any(|tool| Arc::ptr_eq(tool, &activated))
            );
            assert!(Arc::ptr_eq(&parent, &delegate.parent_tools_handle()));
            assert!(
                !parent
                    .read()
                    .iter()
                    .any(|tool| guest_role(tool.as_ref()) && tool.name() == "tool_search")
            );

            // Explicitly AUGMENTED ORIGINAL handle, never called a natural exploit.
            let server2 = late_server(&run, false).await;
            let mut augmented =
                late_build(&run, &["delegate"], false, false, false, |config, _| {
                    late_grant(config, &server2.url)
                })
                .await;
            let delegate2 = augmented
                .built
                .as_ref()
                .unwrap()
                .delegate_tool
                .as_ref()
                .unwrap()
                .clone();
            let parent2 = augmented
                .built
                .as_ref()
                .unwrap()
                .delegate_handle
                .as_ref()
                .unwrap()
                .clone();
            assert!(Arc::ptr_eq(&parent2, &delegate2.parent_tools_handle()));
            parent2.write().push(augmented.guest.clone());
            assert!(
                parent2
                    .read()
                    .iter()
                    .any(|tool| Arc::ptr_eq(tool, &augmented.guest))
            );
            let out2 = late_assemble(&mut augmented, &[], None, true, false, false, None).await;
            server2.handshake();
            let activated2 = late_activate(&run, &out2, &server2).await;
            assert!(
                parent2
                    .read()
                    .iter()
                    .any(|tool| Arc::ptr_eq(tool, &activated2))
            );
            assert!(Arc::ptr_eq(&parent2, &delegate2.parent_tools_handle()));
            late_oracle(
                &run,
                "AUGMENTED-original-parent",
                !parent2
                    .read()
                    .iter()
                    .any(|tool| guest_role(tool.as_ref()) && tool.name() == "tool_search"),
            );
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_deferred_mcp_pipeline_retirement() {
    late_run(
        "late_tool_search_deferred_mcp_pipeline_retirement",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(
                &run,
                &["calculator", "execute_pipeline"],
                true,
                false,
                false,
                |config, _| late_grant(config, &server.url),
            )
            .await;
            let out = late_assemble(&mut case, &[], None, true, false, false, None).await;
            server.handshake();
            assert!(out.tool_search_handle.is_some());
            let pipeline =
                crate::agent::tool_execution::find_tool(&out.registry, "execute_pipeline").unwrap();
            late_pipeline_oracles(&run, pipeline).await;
            assert_eq!(run.trace.count("constructor-return", "pipeline"), 1);
            assert_eq!(run.trace.count("pipeline-publish", "sole"), 1);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_deferred_mcp_skill_direct_retirement() {
    late_run(
        "late_tool_search_deferred_mcp_skill_direct_retirement",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(&run, &[], false, false, false, |config, _| {
                late_grant(config, &server.url)
            })
            .await;
            let skills = [late_skill("late", "invoke", "tool_search")];
            let out = late_assemble(&mut case, &skills, None, true, false, false, None).await;
            server.handshake();
            assert!(out.tool_search_handle.is_some());
            let name = skill_tool::composed_tool_name("late", "invoke");
            if let Some(alias) = crate::agent::tool_execution::find_tool(&out.registry, &name) {
                let witness = alias.execute(late_json!({"text":"direct"})).await.unwrap();
                assert!(witness.success);
                assert_eq!(
                    witness.output.as_str(),
                    "label=unset|uppercase=false|max_len=0|keys=0|text=direct"
                );
            }
            late_oracle(
                &run,
                "direct-skill-target",
                !out.registry.iter().any(|tool| tool.name() == name),
            );
            assert_eq!(run.trace.count("skill-schema-enter", "prepare"), 0);
            assert_eq!(run.trace.count("skill-snapshot", "batch"), 1);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_deferred_mcp_skill_pipeline_retirement() {
    late_run(
        "late_tool_search_deferred_mcp_skill_pipeline_retirement",
        |run| async move {
            for hidden in [false, true] {
                let server = late_server(&run, false).await;
                let mut case = late_build(
                    &run,
                    &["calculator", "execute_pipeline"],
                    true,
                    false,
                    false,
                    |config, security| {
                        late_grant(config, &server.url);
                        if hidden {
                            security.allowed_tools =
                                Some(vec!["calculator".into(), "tool_search".into()]);
                        }
                    },
                )
                .await;
                let mut locked = late_skill("pipeline-skill", "invoke", "execute_pipeline");
                locked.tools[0]
                    .locked_args
                    .insert("result".into(), "last".into());
                let skills = [locked];
                let out = late_assemble(&mut case, &skills, None, true, false, false, None).await;
                server.handshake();
                let alias = crate::agent::tool_execution::find_tool(
                    &out.registry,
                    &skill_tool::composed_tool_name("pipeline-skill", "invoke"),
                )
                .unwrap();
                assert!(alias.requires_unrestricted_principal());
                assert_eq!(
                    out.registry
                        .iter()
                        .any(|tool| tool.name() == "execute_pipeline"),
                    !hidden
                );
                let mut override_attempt = late_pipeline_request("calculator");
                override_attempt["result"] = late_json!("all");
                let locked_result = alias.execute(override_attempt).await.unwrap();
                assert!(locked_result.success);
                assert_eq!(locked_result.output.as_str(), "3");
                late_pipeline_oracles(&run, alias).await;
            }
            let events = run.trace.events();
            let pubs: Vec<_> = events
                .iter()
                .filter(|row| row.kind == "pipeline-publish")
                .collect();
            let binds: Vec<_> = events
                .iter()
                .filter(|row| {
                    row.kind == "skill-bind"
                        && row.label == skill_tool::composed_tool_name("pipeline-skill", "invoke")
                })
                .collect();
            assert_eq!(pubs.len(), 2);
            assert_eq!(binds.len(), 2);
            for (publish, bind) in pubs.iter().zip(binds.iter()) {
                assert_eq!(publish.second, bind.first);
            }
            // PREBUILT first-match equivalence uses an actual earlier pipeline Arc.
            let mut case = late_build(
                &run,
                &["calculator", "execute_pipeline"],
                true,
                false,
                false,
                |_, _| {},
            )
            .await;
            let calc = case
                .built
                .as_ref()
                .unwrap()
                .unfiltered_tool_arcs
                .iter()
                .find(|tool| tool.name() == "calculator")
                .unwrap()
                .clone();
            let prebuilt: Arc<dyn Tool> = Arc::new(PipelineTool::new(
                zeroclaw_config::schema::PipelineConfig {
                    enabled: true,
                    max_steps: 20,
                    allowed_tools: vec!["calculator".into()],
                },
                vec![calc],
            ));
            case.built
                .as_mut()
                .unwrap()
                .unfiltered_tool_arcs
                .insert(0, prebuilt.clone());
            let out = late_assemble(
                &mut case,
                &[late_skill("first", "invoke", "execute_pipeline")],
                None,
                false,
                false,
                false,
                None,
            )
            .await;
            let alias = crate::agent::tool_execution::find_tool(
                &out.registry,
                &skill_tool::composed_tool_name("first", "invoke"),
            )
            .unwrap();
            let result = alias
                .execute(late_pipeline_request("calculator"))
                .await
                .unwrap();
            assert!(result.success);
            assert_eq!(result.output.as_str(), "3");
            let last = run
                .trace
                .events()
                .into_iter()
                .rev()
                .find(|row| {
                    row.kind == "skill-bind"
                        && row.label == skill_tool::composed_tool_name("first", "invoke")
                })
                .unwrap();
            assert_eq!(last.first, late_observer::arc_id(&prebuilt));
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_mcp_search_select_activation() {
    late_run(
        "late_tool_search_mcp_search_select_activation",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(&run, &["delegate"], false, false, false, |config, _| {
                late_grant(config, &server.url)
            })
            .await;
            let parent = case
                .built
                .as_ref()
                .unwrap()
                .delegate_handle
                .as_ref()
                .unwrap()
                .clone();
            let out = late_assemble(
                &mut case,
                &[late_skill("remote", "invoke", "late__allowed")],
                None,
                true,
                false,
                false,
                None,
            )
            .await;
            server.handshake();
            let tool = late_activate(&run, &out, &server).await;
            assert!(parent.read().iter().any(|entry| Arc::ptr_eq(entry, &tool)));
            let remote = crate::agent::tool_execution::find_tool(
                &out.registry,
                &skill_tool::composed_tool_name("remote", "invoke"),
            )
            .unwrap();
            let invoked = remote
                .execute(late_json!({"text":"elevated-call"}))
                .await
                .unwrap();
            assert!(invoked.success);
            assert!(invoked.output.as_str().contains("late-mcp:elevated-call"));
            let denied = out
                .tool_search_handle
                .as_ref()
                .unwrap()
                .execute(late_json!({"query":"select:late__denied"}))
                .await
                .unwrap();
            assert!(!denied.output.as_str().contains("late fixture denied"));
            assert_eq!(server.count("tools/call"), 2);
            assert!(
                !parent
                    .read()
                    .iter()
                    .any(|entry| entry.name() == "tool_search")
            );
        },
    )
    .await;
}

async fn late_last_winner(run: &LateRun) {
    let mut case = late_build(run, &["calculator"], false, true, false, |_, security| {
        security.allowed_tools = Some(
            [
                "shell",
                "file_read",
                "file_write",
                "file_edit",
                "memory_recall",
                "memory_store",
                "tool_search",
            ]
            .iter()
            .map(|s| (*s).into())
            .collect(),
        );
    })
    .await;
    let calc = case
        .built
        .as_ref()
        .unwrap()
        .unfiltered_tool_arcs
        .iter()
        .find(|tool| tool.name() == "calculator")
        .unwrap()
        .clone();
    let first = late_skill("s", "invoke", "calculator");
    // PREBUILT: actual SkillBuiltinTool supplies the existing composed name.
    let prebuilt =
        skill_tool::SkillBuiltinTool::new("s", &first.tools[0], calc, Default::default());
    case.built.as_mut().unwrap().tools.push(Box::new(prebuilt));
    let out = late_assemble(
        &mut case,
        &[first, late_skill("s", "invoke", "tool_search")],
        None,
        false,
        false,
        false,
        None,
    )
    .await;
    assert!(out.tool_search_handle.is_some());
    let name = skill_tool::composed_tool_name("s", "invoke");
    let aliases: Vec<_> = out
        .registry
        .iter()
        .filter(|tool| tool.name() == name)
        .collect();
    assert!(!aliases.is_empty());
    let real = aliases[0]
        .execute(late_json!({"function":"add","values":[1,2]}))
        .await
        .unwrap();
    assert!(real.success);
    assert_eq!(real.output.as_str(), "3");
    late_oracle(
        run,
        "last-winner-not-promoted",
        !out.registry.hidden_builtin_names().contains(&name),
    );
    late_oracle(run, "retired-last-alias", aliases.len() == 1);
    let selected = out
        .tool_search_handle
        .as_ref()
        .unwrap()
        .execute(late_json!({"query":format!("select:{name}")}))
        .await
        .unwrap();
    late_oracle(
        run,
        "retired-last-schema",
        !selected.output.as_str().contains("late fixture alias"),
    );
    let events = run.trace.events();
    let specs: Vec<_> = events
        .iter()
        .filter(|row| row.kind == "captured-spec" && row.label == name)
        .collect();
    assert_eq!(specs.len(), 2);
    assert_ne!(specs[0].second, specs[1].second);
    assert!(
        out.activated_handle
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .hidden_builtin_names()
            .is_empty()
    );
}

#[tokio::test]
async fn late_tool_search_builtin_only_views() {
    late_run("late_tool_search_builtin_only_views", |run| async move {
        let mut case = late_build(
            &run,
            &["delegate", "calculator", "execute_pipeline"],
            true,
            true,
            false,
            |_, _| {},
        )
        .await;
        let delegate = case
            .built
            .as_ref()
            .unwrap()
            .delegate_tool
            .as_ref()
            .unwrap()
            .clone();
        let parent = case
            .built
            .as_ref()
            .unwrap()
            .delegate_handle
            .as_ref()
            .unwrap()
            .clone();
        assert!(Arc::ptr_eq(&parent, &delegate.parent_tools_handle()));
        assert!(
            !parent
                .read()
                .iter()
                .any(|tool| guest_role(tool.as_ref()) && tool.name() == "tool_search")
        );
        parent.write().push(case.guest.clone()); // Explicit AUGMENTED original handle.
        let out = late_assemble(
            &mut case,
            &[
                late_skill("direct", "invoke", "tool_search"),
                late_skill("nested", "invoke", "execute_pipeline"),
            ],
            None,
            false,
            false,
            false,
            None,
        )
        .await;
        assert!(out.tool_search_handle.is_some());
        late_guest_absent(&run, &out);
        late_oracle(
            &run,
            "builtin-original-parent",
            !parent
                .read()
                .iter()
                .any(|tool| guest_role(tool.as_ref()) && tool.name() == "tool_search"),
        );
        late_oracle(
            &run,
            "builtin-direct-alias",
            !out.registry
                .iter()
                .any(|tool| tool.name() == skill_tool::composed_tool_name("direct", "invoke")),
        );
        let alias = crate::agent::tool_execution::find_tool(
            &out.registry,
            &skill_tool::composed_tool_name("nested", "invoke"),
        )
        .unwrap();
        late_pipeline_oracles(&run, alias).await;
        assert_eq!(run.trace.count("constructor-return", "builtin-search"), 1);
        assert_eq!(run.trace.count("constructor-return", "mcp-search"), 0);
        assert_eq!(run.trace.count("mcp-connect", "actual"), 0);
        assert_eq!(run.trace.count("skill-schema-enter", "prepare"), 2);
    })
    .await;
}

#[tokio::test]
async fn late_tool_search_builtin_only_schema_selection() {
    late_run(
        "late_tool_search_builtin_only_schema_selection",
        |run| async move {
            let mut case = late_build(&run, &["calculator"], false, true, false, |_, _| {}).await;
            let out = late_assemble(&mut case, &[], None, false, false, false, None).await;
            assert!(out.registry.hidden_builtin_names().contains("calculator"));
            let before = run.trace.count("constructor-return", "calculator");
            let selected = out
                .tool_search_handle
                .as_ref()
                .unwrap()
                .execute(late_json!({"query":"select:calculator"}))
                .await
                .unwrap();
            assert!(selected.success);
            assert!(selected.output.as_str().contains("calculator"));
            assert!(!out.registry.hidden_builtin_names().contains("calculator"));
            assert_eq!(run.trace.count("constructor-return", "calculator"), before);
            late_last_winner(&run).await;
        },
    )
    .await;
}

async fn late_no_host_case(run: LateRun, row: usize) {
    let server = late_server(&run, row == 5).await;
    let mut case = late_build(&run, &[], false, row == 7, false, |config, security| {
        late_grant(config, &server.url);
        match row {
            0 => config.mcp.enabled = false,
            2 => config
                .agents
                .get_mut("namespace")
                .unwrap()
                .mcp_bundles
                .clear(),
            3 => config.mcp.deferred_loading = false,
            6 => {
                security.excluded_tools = Some(vec!["late__allowed".into(), "late__denied".into()])
            }
            7 => {
                config.mcp.enabled = false;
                security.allowed_tools = Some(
                    [
                        "shell",
                        "file_read",
                        "file_write",
                        "file_edit",
                        "memory_recall",
                        "memory_store",
                        "tool_search",
                    ]
                    .iter()
                    .map(|s| (*s).into())
                    .collect(),
                );
            }
            _ => {}
        }
    })
    .await;
    let out = late_assemble(&mut case, &[], None, row != 1, row == 4, false, None).await;
    assert!(out.tool_search_handle.is_none(), "no-owner row {row}");
    let guest = crate::agent::tool_execution::find_tool(&out.registry, "tool_search").unwrap();
    assert!(guest_role(guest));
    let value = guest.execute(late_json!({"text":"kept"})).await.unwrap();
    assert!(value.success);
    assert_eq!(
        value.output.as_str(),
        "label=unset|uppercase=false|max_len=0|keys=0|text=kept"
    );
    if [0, 1, 2, 7].contains(&row) {
        assert!(server.methods().is_empty());
    } else {
        server.handshake();
    }
    if [3, 4].contains(&row) {
        assert!(
            out.registry
                .iter()
                .any(|tool| tool.name() == "late__allowed")
        );
    }
    assert_eq!(run.trace.count("constructor-return", "mcp-search"), 0);
    assert_eq!(run.trace.count("constructor-return", "builtin-search"), 0);
}

#[tokio::test]
async fn late_tool_search_no_host_disabled_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_disabled_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 0).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_connect_disabled_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_connect_disabled_keeps_real_guest",
        |run| async move { late_no_host_case(run, 1).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_ungranted_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_ungranted_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 2).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_eager_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_eager_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 3).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_listing_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_listing_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 4).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_empty_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_empty_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 5).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_filtered_mcp_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_filtered_mcp_keeps_real_guest",
        |run| async move { late_no_host_case(run, 6).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_no_host_empty_builtin_catalog_keeps_real_guest() {
    late_run(
        "late_tool_search_no_host_empty_builtin_catalog_keeps_real_guest",
        |run| async move { late_no_host_case(run, 7).await },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_helper_denied_keeps_no_guest_recovery() {
    late_run(
        "late_tool_search_helper_denied_keeps_no_guest_recovery",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(
                &run,
                &["calculator", "execute_pipeline"],
                true,
                true,
                false,
                |config, security| {
                    late_grant(config, &server.url);
                    security.excluded_tools =
                        Some(vec!["late__denied".into(), "tool_search".into()]);
                },
            )
            .await;
            let out = late_assemble(
                &mut case,
                &[late_skill("late", "invoke", "tool_search")],
                None,
                true,
                false,
                false,
                None,
            )
            .await;
            server.handshake();
            assert!(out.tool_search_handle.is_some());
            assert!(!out.registry.iter().any(|tool| tool.name() == "tool_search"));
            assert!(out.deferred_section().is_empty());
            let alias = skill_tool::composed_tool_name("late", "invoke");
            late_oracle(
                &run,
                "denied-helper-direct-capture",
                !out.registry.iter().any(|tool| tool.name() == alias),
            );
            assert_eq!(run.trace.count("constructor-return", "builtin-search"), 0);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_caller_principal_context() {
    late_run("late_tool_search_caller_principal_context", |run| async move {
        let mut case=late_build(&run,&["calculator","execute_pipeline","deliver_file"],true,true,false,|config,_| { config.pipeline.allowed_tools.extend(["memory_recall".into(),"deliver_file".into()]); }).await;
        let original_children:Vec<_>=case.built.as_ref().unwrap().unfiltered_tool_arcs.iter().filter(|tool|["memory_recall","deliver_file"].contains(&tool.name())).map(late_observer::arc_id).collect();
        assert_eq!(original_children.len(),2);
        let caller=vec!["calculator".into(),"execute_pipeline".into(),"tool_search".into(),"memory_recall".into(),"deliver_file".into()];
        let out=late_assemble(&mut case,&[late_skill("nested","invoke","execute_pipeline")],Some(&caller),false,false,true,None).await;
        assert!(!out.registry.iter().any(|tool|zeroclaw_tools::MEMORY_TOOL_NAMES.contains(&tool.name()) || tool.name()=="deliver_file"));
        assert!(run.trace.events().iter().filter(|row|row.kind=="context-entry").all(|row|!original_children.contains(&row.first)));
        let pipeline=crate::agent::tool_execution::find_tool(&out.registry,"execute_pipeline").unwrap();
        assert!(pipeline.requires_unrestricted_principal());
        for child in ["memory_recall","deliver_file"] { assert!(!pipeline.execute(late_json!({"steps":[{"tool":child,"args":{}}],"result":"last"})).await.unwrap().success); }
        let mut config=case.config.clone();
        let bundle_dir=config.install_root_dir().join("shared/skills/late-principal");
        let skill_dir=bundle_dir.join("nested"); std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.toml"), "[skill]\nname = \"nested\"\ndescription = \"bounded principal fixture\"\n[[tools]]\nname = \"invoke\"\ndescription = \"existing pipeline\"\nkind = \"builtin\"\ncommand = \"\"\ntarget = \"execute_pipeline\"\n").unwrap();
        config.skill_bundles.insert("late-principal".into(),zeroclaw_config::schema::SkillBundleConfig { directory:Some(bundle_dir.display().to_string()),..Default::default() });
        config.agents.get_mut("namespace").unwrap().skill_bundles=vec!["late-principal".into()];
        let loaded=crate::skills::load_skills_for_agent_from_config(&config,"namespace");
        assert_eq!(loaded.len(),1); assert_eq!(loaded[0].name,"nested");
        let mut unrestricted=crate::agent::Agent::from_live_config_with_tui_env_and_principal_tools(zeroclaw_config::live::LiveConfig::new(config.clone()).handle(),"namespace",None,false,true,None,None,None,None).await.unwrap();
        assert!(unrestricted.tool_names().contains(&"execute_pipeline"));
        assert!(unrestricted.tool_names().contains(&skill_tool::composed_tool_name("nested","invoke").as_str()));
        let mut principal_names=caller.clone(); principal_names.push(skill_tool::composed_tool_name("nested","invoke"));
        unrestricted.narrow_to_principal_tools(Some(&principal_names));
        assert!(!unrestricted.tool_names().contains(&"execute_pipeline"));
        assert!(!unrestricted.tool_names().contains(&skill_tool::composed_tool_name("nested","invoke").as_str()));
        let mut agent=crate::agent::Agent::from_live_config_with_tui_env_and_principal_tools(zeroclaw_config::live::LiveConfig::new(config).handle(),"namespace",None,false,true,None,None,None,Some(principal_names)).await.unwrap();
        assert!(!agent.tool_names().contains(&"execute_pipeline"));
        assert!(!agent.tool_names().contains(&skill_tool::composed_tool_name("nested","invoke").as_str()));
        agent.narrow_to_principal_tools(Some(&["calculator".into()]));
        assert!(agent.tool_names().iter().all(|name|*name=="calculator"));
    }).await;
}

#[tokio::test]
async fn late_tool_search_recovery_cannot_reintroduce_guest() {
    late_run(
        "late_tool_search_recovery_cannot_reintroduce_guest",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(
                &run,
                &["calculator", "execute_pipeline"],
                true,
                true,
                false,
                |config, _| late_grant(config, &server.url),
            )
            .await;
            let mut out = late_assemble(&mut case, &[], None, true, false, false, None).await;
            server.handshake();
            let activated = late_activate(&run, &out, &server).await;
            let selected = out
                .tool_search_handle
                .as_ref()
                .unwrap()
                .execute(late_json!({"query":"select:calculator"}))
                .await
                .unwrap();
            assert!(selected.success);
            let state = out.activated_handle.as_ref().unwrap().clone();
            let poison = state.clone();
            assert!(
                std::thread::spawn(move || {
                    let _guard = poison.lock().unwrap();
                    panic!("intentional owned activation poison");
                })
                .join()
                .is_err()
            );
            let allowed = vec![
                "tool_search".into(),
                "execute_pipeline".into(),
                "late__allowed".into(),
            ];
            out.tool_search_handle
                .as_ref()
                .unwrap()
                .narrow_to_caller(&allowed);
            out.registry
                .retain(|tool| allowed.iter().any(|name| name == tool.name()));
            assert!(Arc::ptr_eq(
                &state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .get("late__allowed")
                    .unwrap(),
                &activated
            ));
            assert!(
                !state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .is_activated("late__denied")
            );
            assert!(!out.registry.hidden_builtin_names().contains("calculator"));
            let pipeline =
                crate::agent::tool_execution::find_tool(&out.registry, "execute_pipeline").unwrap();
            late_pipeline_oracles(&run, pipeline).await;
            assert_eq!(server.count("initialize"), 1);
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_shared_registry_no_reconnect() {
    late_run(
        "late_tool_search_shared_registry_no_reconnect",
        |run| async move {
            let server = late_server(&run, false).await;
            let mut case = late_build(&run, &[], false, false, false, |config, _| {
                late_grant(config, &server.url)
            })
            .await;
            let shared = Arc::new(
                McpRegistry::connect_all(&case.config.mcp_servers_for_agent("namespace"))
                    .await
                    .unwrap(),
            );
            assert_eq!(shared.server_count(), 1);
            let names = shared.tool_names();
            assert!(
                names.contains(&"late__allowed".into()) && names.contains(&"late__denied".into())
            );
            server.handshake();
            let expected = late_observer::arc_id(&shared);
            let out = late_assemble(&mut case, &[], None, true, false, false, Some(shared)).await;
            late_activate(&run, &out, &server).await;
            assert_eq!(run.trace.count("mcp-connect", "actual"), 0);
            assert!(
                run.trace
                    .events()
                    .iter()
                    .any(|row| row.kind == "mcp-registry"
                        && row.label == "used"
                        && row.first == expected
                        && row.second == 1)
            );
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_minimal_full_compiled_semantics() {
    late_run(
        "late_tool_search_minimal_full_compiled_semantics",
        |run| async move {
            let case = late_build(&run, &[], true, false, false, |_, _| {}).await;
            let built = case.built.as_ref().unwrap();
            let names: std::collections::BTreeSet<_> = built
                .tools
                .iter()
                .filter(|tool| !guest_role(tool.as_ref()))
                .map(|tool| tool.name())
                .collect();
            let core: std::collections::BTreeSet<_> =
                zeroclaw_config::builtin_tools::CORE_TOOL_NAMES
                    .iter()
                    .copied()
                    .collect();
            assert_eq!(names, core);
            assert!(
                built.delegate_handle.is_none()
                    && built.ask_user_handle.is_none()
                    && built.poll_handle.is_none()
            );
            assert!(!case.tmp.path().join("unselected-knowledge.db").exists());
            assert_eq!(run.trace.count("constructor-enter", "calculator"), 0);
            assert_eq!(run.trace.count("constructor-enter", "knowledge"), 0);
            let upgraded =
                zeroclaw_config::migration::migrate_to_current("schema_version = 2\n").unwrap();
            assert_eq!(upgraded.tools.optional, vec!["*".to_owned()]);
            let fresh = Config::default();
            assert_eq!(fresh.tools.optional, vec!["*".to_owned()]);
            let saved = toml::to_string(&fresh).unwrap();
            let roundtrip: Config = toml::from_str(&saved).unwrap();
            assert_eq!(roundtrip.tools.optional, vec!["*".to_owned()]);
            let mut selected = Config::default();
            selected.tools.optional = vec!["weather".into()];
            let warnings = selected.collect_warnings();
            assert_eq!(
                warnings.iter().any(|warning| warning.code
                    == zeroclaw_config::validation_warnings::TOOL_COMPILED_OUT
                    && warning.path == "tools.optional"
                    && warning.message.contains("weather")),
                !cfg!(feature = "tools-external")
            );
        },
    )
    .await;
}

#[tokio::test]
async fn late_tool_search_registration_order_and_hidden_specs() {
    late_run(
        "late_tool_search_registration_order_and_hidden_specs",
        |run| async move {
            let mut case = late_build(&run, &["calculator"], false, false, false, |_, _| {}).await;
            let mut mixed = late_skill("mixed", "invoke", "calculator");
            for (name, kind, target) in [
                ("shell", "shell", None),
                ("script", "script", None),
                ("http", "http", None),
                ("mcp", "mcp", Some("missing__tool")),
                ("missing", "builtin", Some("missing")),
                ("unknown", "unknown", None),
            ] {
                let mut entry = mixed.tools[0].clone();
                entry.name = name.into();
                entry.kind = kind.into();
                entry.command = if kind == "http" {
                    "https://example.invalid".into()
                } else {
                    "echo never-executed".into()
                };
                entry.target = target.map(str::to_owned);
                mixed.tools.push(entry);
            }
            let same = late_skill("same", "invoke", "calculator");
            let out = late_assemble(
                &mut case,
                &[mixed, same.clone(), same],
                None,
                false,
                false,
                false,
                None,
            )
            .await;
            let composed = skill_tool::composed_tool_name("same", "invoke");
            assert_eq!(
                out.registry
                    .iter()
                    .filter(|tool| tool.name() == composed)
                    .count(),
                2
            );
            let events = run.trace.events();
            let snapshot = events
                .iter()
                .position(|row| row.kind == "skill-snapshot")
                .unwrap();
            assert!(
                events
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| row.kind == "skill-constructor-enter"
                        || row.kind == "skill-schema-enter")
                    .all(|(index, _)| index < snapshot)
            );
            assert_eq!(run.trace.count("skill-snapshot", "batch"), 1);
            // Invalid-NAT64 scoped input only: factory admission/preflight was valid.
            let mut invalid =
                late_build(&run, &["calculator"], false, false, false, |_, _| {}).await;
            invalid.config.security.nat64_prefixes = vec!["not-a-prefix".into()];
            let mut http = late_skill("invalid", "http", "calculator");
            http.tools[0].kind = "http".into();
            http.tools[0].command = "https://example.invalid".into();
            let out = late_assemble(
                &mut invalid,
                &[http, late_skill("valid", "calc", "calculator")],
                None,
                false,
                false,
                false,
                None,
            )
            .await;
            assert!(
                !out.registry
                    .iter()
                    .any(|tool| tool.name() == skill_tool::composed_tool_name("invalid", "http"))
            );
            assert!(
                out.registry
                    .iter()
                    .any(|tool| tool.name() == skill_tool::composed_tool_name("valid", "calc"))
            );
            // The original fixed snapshot and decision precedence are observable
            // without introducing synthetic name/role stand-ins.
            for decision in ["duplicate", "deny-all", "excluded"] {
                let before = run.trace.count("skill-decision", decision);
                let mut row =
                    late_build(&run, &["calculator"], false, false, false, |_, security| {
                        if decision == "deny-all" {
                            security.allowed_tools = Some(Vec::new());
                        }
                        if decision == "excluded" {
                            security.excluded_tools =
                                Some(vec![skill_tool::composed_tool_name("guarded", "invoke")]);
                        }
                    })
                    .await;
                let declared = late_skill("guarded", "invoke", "calculator");
                if decision == "duplicate" {
                    let target = row
                        .built
                        .as_ref()
                        .unwrap()
                        .unfiltered_tool_arcs
                        .iter()
                        .find(|tool| tool.name() == "calculator")
                        .unwrap()
                        .clone();
                    row.built.as_mut().unwrap().tools.push(Box::new(
                        skill_tool::SkillBuiltinTool::new(
                            "guarded",
                            &declared.tools[0],
                            target,
                            Default::default(),
                        ),
                    ));
                }
                let out = late_assemble(
                    &mut row,
                    &[declared.clone(), declared],
                    None,
                    false,
                    false,
                    false,
                    None,
                )
                .await;
                assert_eq!(run.trace.count("skill-decision", decision) - before, 2);
                let count = out
                    .registry
                    .iter()
                    .filter(|tool| {
                        tool.name() == skill_tool::composed_tool_name("guarded", "invoke")
                    })
                    .count();
                assert_eq!(count, if decision == "duplicate" { 1 } else { 0 });
            }
            late_last_winner(&run).await;
        },
    )
    .await;
}
