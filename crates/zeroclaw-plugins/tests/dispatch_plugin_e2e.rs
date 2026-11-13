//! Real Component Model coverage for the tool world's `dispatch` import.
//!
//! A compiled tool component (`tests/fixtures/tool-dispatch-fixture`) calls
//! `zeroclaw:plugin/dispatch.wake` through the tool runtime exactly as a
//! deployed plugin would. Reach comes only from the `agent_dispatch` grant: a
//! scope without it does not even get a linker import to bind, so the component
//! fails to instantiate rather than failing at call time. The caller alias is
//! host-owned and never guest-supplied, and every request is shape-validated
//! before the runtime's wake backend runs.

#![cfg(feature = "plugins-wasm-cranelift")]

mod support;

use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use zeroclaw_api::tool::Tool as _;
use zeroclaw_plugins::component::PluginLimits;
use zeroclaw_plugins::config::{PluginConfigResolver, resolve_plugin_config};
use zeroclaw_plugins::instance::PluginInstanceScope;
use zeroclaw_plugins::services::{
    AgentWakeBackend, AgentWakeError, AgentWakeService, PluginHostServices,
};
use zeroclaw_plugins::wasm_tool::WasmTool;
use zeroclaw_plugins::{PluginCapability, PluginManifest, PluginPermission};

use support::{admit_fixture, state_service};

fn fixture() -> PathBuf {
    static FIXTURE: OnceLock<PathBuf> = OnceLock::new();
    FIXTURE
        .get_or_init(|| {
            let fixture_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/tool-dispatch-fixture");
            let target_dir =
                PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("tool-dispatch-plugin-fixture");
            let status = Command::new(env!("CARGO"))
                .current_dir(&fixture_dir)
                .args([
                    "build",
                    "--locked",
                    "--quiet",
                    "--package",
                    "zeroclaw-tool-dispatch-plugin-fixture",
                    "--target",
                    "wasm32-wasip2",
                    "--target-dir",
                ])
                .arg(&target_dir)
                .status()
                .expect("run Cargo for the tool dispatch component fixture");
            assert!(
                status.success(),
                "tool dispatch fixture must build; install the wasm32-wasip2 target"
            );
            let wasm =
                target_dir.join("wasm32-wasip2/debug/zeroclaw_tool_dispatch_plugin_fixture.wasm");
            assert!(
                wasm.is_file(),
                "tool dispatch fixture WASM was not produced"
            );
            wasm
        })
        .clone()
}

fn limits() -> PluginLimits {
    PluginLimits {
        call_fuel: 1_000_000_000,
        max_memory_bytes: 64 * 1024 * 1024,
        max_table_elements: 10_000,
        max_instances: 10,
        call_timeout: std::time::Duration::from_secs(20),
    }
}

/// A manifest that requests `agent_dispatch` only when `requests_dispatch` is
/// set. The guest always imports `dispatch`, so the manifest flag decides what
/// the host would be allowed to grant, and the scope decides what it did.
fn manifest(requests_dispatch: bool) -> PluginManifest {
    let permissions = if requests_dispatch {
        vec![PluginPermission::AgentDispatch]
    } else {
        Vec::new()
    };
    PluginManifest {
        name: "tool-dispatch-fixture".to_string(),
        version: "0.0.0".to_string(),
        description: None,
        author: None,
        wasm_path: Some("tool-dispatch-fixture.wasm".to_string()),
        wasm_sha256: None,
        capabilities: vec![PluginCapability::Tool],
        provides: None,
        permissions,
        config_schema: None,
        signature: None,
        publisher_key: None,
        egress: Default::default(),
    }
}

/// One recorded [`RecordingWake`] call: the exact host-owned caller alias, the
/// guest's target, and the guest's prompt.
#[derive(Clone, Debug, PartialEq, Eq)]
struct RecordedWake {
    caller_alias: Option<String>,
    target_agent: String,
    prompt: String,
}

/// A wake backend that records what it received and answers with a fixed
/// outcome, so a test can assert both forwarding and category mapping.
#[derive(Clone)]
struct RecordingWake {
    calls: Arc<Mutex<Vec<RecordedWake>>>,
    outcome: Result<(), AgentWakeError>,
}

impl RecordingWake {
    fn accepting() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome: Ok(()),
        }
    }

    fn refusing(error: AgentWakeError) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            outcome: Err(error),
        }
    }

    fn recorded(&self) -> Vec<RecordedWake> {
        self.calls.lock().expect("recorded wakes").clone()
    }

    fn call_count(&self) -> usize {
        self.calls.lock().expect("recorded wakes").len()
    }
}

#[async_trait]
impl AgentWakeBackend for RecordingWake {
    async fn wake(
        &self,
        _scope: &PluginInstanceScope,
        caller_alias: Option<&str>,
        target_agent: &str,
        prompt: &str,
    ) -> Result<(), AgentWakeError> {
        self.calls
            .lock()
            .expect("recorded wakes")
            .push(RecordedWake {
                caller_alias: caller_alias.map(ToOwned::to_owned),
                target_agent: target_agent.to_string(),
                prompt: prompt.to_string(),
            });
        self.outcome
    }
}

/// Build the tool for one admission decision: what the manifest requested, what
/// the scope granted, and which backend serves dispatch.
fn tool(
    binding: &str,
    requests_dispatch: bool,
    grants: Vec<PluginPermission>,
    backend: &RecordingWake,
) -> anyhow::Result<WasmTool> {
    let manifest = manifest(requests_dispatch);
    let scope =
        PluginInstanceScope::from_manifest(&manifest, PluginCapability::Tool, binding, grants)
            .expect("admit fixture scope");
    let resolver_manifest = manifest.clone();
    let services = PluginHostServices::new(
        PluginConfigResolver::new(move |scope| {
            resolve_plugin_config(&resolver_manifest, scope, None)
        }),
        state_service(),
    )
    .with_wake(AgentWakeService::new(backend.clone()));
    let component = admit_fixture(&fixture(), &manifest);
    WasmTool::from_wasm(component, scope, services, limits(), None)
}

#[tokio::test]
async fn dispatch_granted_invokes_wake_backend_with_caller_alias() {
    let backend = RecordingWake::accepting();
    let tool = tool(
        "alpha",
        true,
        vec![PluginPermission::AgentDispatch],
        &backend,
    )
    .expect("a dispatch-granted scope instantiates the importing component")
    .with_caller_alias("alpha");
    assert_eq!(tool.name(), "dispatch_fixture");

    let result = tool
        .execute(serde_json::json!({"target": "beta", "prompt": "check crossbar"}))
        .await
        .expect("guest returns a tool result, not a trap");

    assert!(result.success, "the admitted wake must report success");
    assert_eq!(result.output.as_str(), "ok");
    assert_eq!(result.error, None);
    assert_eq!(
        backend.recorded(),
        vec![RecordedWake {
            caller_alias: Some("alpha".to_string()),
            target_agent: "beta".to_string(),
            prompt: "check crossbar".to_string(),
        }],
        "the store must forward its host-owned alias and the guest's request unchanged"
    );
}

#[tokio::test]
async fn dispatch_without_permission_fails_instantiation() {
    let backend = RecordingWake::accepting();
    let error = tool("alpha", false, Vec::new(), &backend)
        .err()
        .expect("a component importing dispatch cannot load without the grant");

    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("failed to instantiate"),
        "the failure must be the instantiation, not a call: {rendered}"
    );
    assert!(
        rendered.contains("zeroclaw:plugin/dispatch"),
        "the unlinked import must be named: {rendered}"
    );
    assert_eq!(
        backend.call_count(),
        0,
        "a component that never instantiated cannot have dispatched"
    );
}

#[tokio::test]
async fn dispatch_granted_in_manifest_but_withheld_by_scope_fails_instantiation() {
    let backend = RecordingWake::accepting();
    let error = tool("alpha", true, Vec::new(), &backend).err().expect(
        "a manifest request the scope withheld must leave the import unlinked and uninstantiable",
    );

    let rendered = format!("{error:#}");
    assert!(
        rendered.contains("failed to instantiate"),
        "the withheld grant must fail at instantiation: {rendered}"
    );
    assert!(
        rendered.contains("zeroclaw:plugin/dispatch"),
        "the withheld import must be the one named: {rendered}"
    );
    assert_eq!(
        backend.call_count(),
        0,
        "an uninstantiated component cannot reach the backend"
    );
}

/// Assert the guest reported one exact `DispatchError` case.
///
/// The fixture returns the error's `Debug` form, and wit-bindgen renders a
/// generated enum variant as `DispatchError { code, name: "<wit-case>", .. }`,
/// so the WIT case name is the identifier that actually crosses the boundary.
fn assert_dispatch_error(error: &str, wit_case: &str) {
    assert!(
        error.contains(&format!("name: \"{wit_case}\"")),
        "guest must observe the {wit_case} dispatch error, got: {error}"
    );
}

#[tokio::test]
async fn dispatch_rejects_invalid_prompt_before_backend() {
    let backend = RecordingWake::accepting();
    let tool = tool(
        "alpha",
        true,
        vec![PluginPermission::AgentDispatch],
        &backend,
    )
    .expect("a dispatch-granted scope instantiates the importing component")
    .with_caller_alias("alpha");

    for prompt in ["".to_string(), "bad\0prompt".to_string()] {
        let result = tool
            .execute(serde_json::json!({"target": "beta", "prompt": prompt}))
            .await
            .expect("guest returns a tool result, not a trap");
        assert!(
            !result.success,
            "{prompt:?} must be refused by the host, not delivered"
        );
        let error = result.error.expect("a refused wake reports its category");
        assert_dispatch_error(&error, "invalid-request");
    }

    assert_eq!(
        backend.call_count(),
        0,
        "a rejected request must never reach the backend"
    );
}

#[tokio::test]
async fn dispatch_forwards_backend_error_variants() {
    for (backend_error, wit_case) in [
        (AgentWakeError::AccessDenied, "access-denied"),
        (AgentWakeError::RateLimited, "rate-limited"),
        (AgentWakeError::UnknownAgent, "unknown-agent"),
        (AgentWakeError::Unavailable, "unavailable"),
    ] {
        let backend = RecordingWake::refusing(backend_error);
        let tool = tool(
            "alpha",
            true,
            vec![PluginPermission::AgentDispatch],
            &backend,
        )
        .expect("a dispatch-granted scope instantiates the importing component")
        .with_caller_alias("alpha");

        let result = tool
            .execute(serde_json::json!({"target": "beta", "prompt": "check crossbar"}))
            .await
            .expect("guest returns a tool result, not a trap");

        assert!(
            !result.success,
            "{backend_error:?} must be reported as a failed tool result"
        );
        let error = result.error.expect("a refused wake reports its category");
        assert_dispatch_error(&error, wit_case);
        assert_eq!(
            backend.call_count(),
            1,
            "the backend answered the one admitted call"
        );
    }
}
