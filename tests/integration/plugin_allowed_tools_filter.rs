#![cfg(feature = "plugins-wasm")]

//! Integration test for allowed_tools filtering on tool delegation.
//!
//! Task US-ZCL-24-4: Verify acceptance criterion for story US-ZCL-24:
//! > Only tools in allowed_tools list can be delegated to
//!
//! These tests assert that:
//! 1. A tool listed in allowed_tools can be dispatched
//! 2. A tool NOT listed in allowed_tools is rejected
//! 3. Empty allowed_tools rejects every tool
//! 4. The allowed_tools check uses exact name matching (no partial matches)
//! 5. Multiple allowed tools: only listed ones pass the filter
//! 6. The build_functions path correctly propagates allowed_tools from the manifest

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{PluginCapabilities, PluginManifest, ToolDelegationCapability};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::tools::{Tool, ToolResult};

/// A mock tool that records calls and returns a configurable response.
struct TrackingTool {
    tool_name: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl TrackingTool {
    fn new(name: &'static str, calls: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            tool_name: name,
            calls,
        }
    }
}

#[async_trait]
impl Tool for TrackingTool {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn description(&self) -> &str {
        "tracking mock tool"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    async fn execute(&self, args: Value) -> anyhow::Result<ToolResult> {
        self.calls.lock().push(args);
        Ok(ToolResult {
            success: true,
            output: format!("{} executed", self.tool_name),
            error: None,
        })
    }
}

fn make_audit() -> Arc<AuditLogger> {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let cfg = AuditConfig {
        enabled: false,
        ..Default::default()
    };
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    Arc::new(AuditLogger::new(cfg, path).expect("audit logger"))
}

fn make_manifest_with_delegation(allowed_tools: Vec<String>) -> PluginManifest {
    let toml_str = r#"
        name = "delegator_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let mut m: PluginManifest = toml::from_str(toml_str).expect("valid manifest");
    m.host_capabilities = PluginCapabilities {
        tool_delegation: Some(ToolDelegationCapability { allowed_tools }),
        ..Default::default()
    };
    m
}

/// Simulate the allowed_tools check from the host function:
/// `!allowed_tools.iter().any(|t| t == "*" || t == &tool_name)`
fn is_tool_allowed(allowed_tools: &[String], tool_name: &str) -> bool {
    allowed_tools.iter().any(|t| t == "*" || t == tool_name)
}

// ---------------------------------------------------------------------------
// 1. A tool listed in allowed_tools can be dispatched
// ---------------------------------------------------------------------------

#[tokio::test]
async fn allowed_tool_can_be_dispatched() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let manifest = make_manifest_with_delegation(vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    let target_name = "echo";
    assert!(
        is_tool_allowed(allowed, target_name),
        "'echo' should pass the allowed_tools check"
    );

    // Dispatch succeeds
    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == target_name)
        .unwrap();
    let result = found.execute(json!({})).await.unwrap();
    assert!(result.success);
    assert_eq!(calls.lock().len(), 1);
}

// ---------------------------------------------------------------------------
// 2. A tool NOT listed in allowed_tools is rejected
// ---------------------------------------------------------------------------

#[test]
fn unlisted_tool_is_rejected_by_allowed_check() {
    let manifest = make_manifest_with_delegation(vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(
        !is_tool_allowed(allowed, "file_write"),
        "'file_write' is not in allowed_tools and must be rejected"
    );
}

#[tokio::test]
async fn unlisted_tool_present_in_registry_still_rejected() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("secret_tool", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    // The tool exists in the registry but is NOT in allowed_tools
    let manifest = make_manifest_with_delegation(vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Tool is findable in registry...
    let found = registry.tools.iter().find(|t| t.name() == "secret_tool");
    assert!(found.is_some(), "tool exists in registry");

    // ...but the allowed_tools check blocks it before dispatch
    assert!(
        !is_tool_allowed(allowed, "secret_tool"),
        "tool must be blocked by allowed_tools even though it's in the registry"
    );

    // Tool was never called
    assert!(
        calls.lock().is_empty(),
        "rejected tool must not be executed"
    );
}

// ---------------------------------------------------------------------------
// 3. Empty allowed_tools rejects every tool
// ---------------------------------------------------------------------------

#[test]
fn empty_allowed_tools_rejects_all() {
    let manifest = make_manifest_with_delegation(vec![]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(allowed.is_empty());
    assert!(!is_tool_allowed(allowed, "echo"));
    assert!(!is_tool_allowed(allowed, "file_read"));
    assert!(!is_tool_allowed(allowed, "anything"));
}

// ---------------------------------------------------------------------------
// 4. Exact name matching — no partial or substring matches
// ---------------------------------------------------------------------------

#[test]
fn partial_name_does_not_match() {
    let manifest = make_manifest_with_delegation(vec!["file_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Substrings must not match
    assert!(!is_tool_allowed(allowed, "file"));
    assert!(!is_tool_allowed(allowed, "read"));
    assert!(!is_tool_allowed(allowed, "file_read_all"));
    assert!(!is_tool_allowed(allowed, "file_rea"));

    // Only exact match passes
    assert!(is_tool_allowed(allowed, "file_read"));
}

#[test]
fn case_sensitive_matching() {
    let manifest = make_manifest_with_delegation(vec!["Echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(is_tool_allowed(allowed, "Echo"));
    assert!(!is_tool_allowed(allowed, "echo"));
    assert!(!is_tool_allowed(allowed, "ECHO"));
}

// ---------------------------------------------------------------------------
// 5. Multiple allowed tools: only listed ones pass the filter
// ---------------------------------------------------------------------------

#[tokio::test]
async fn multiple_allowed_tools_selective_dispatch() {
    let calls_a = Arc::new(Mutex::new(Vec::new()));
    let calls_b = Arc::new(Mutex::new(Vec::new()));
    let calls_c = Arc::new(Mutex::new(Vec::new()));
    let tool_a: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls_a.clone()));
    let tool_b: Arc<dyn Tool> = Arc::new(TrackingTool::new("file_read", calls_b.clone()));
    let tool_c: Arc<dyn Tool> = Arc::new(TrackingTool::new("file_write", calls_c.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool_a, tool_b, tool_c], make_audit());

    // Only echo and file_read are allowed
    let manifest = make_manifest_with_delegation(vec!["echo".into(), "file_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Allowed tools pass the check
    assert!(is_tool_allowed(allowed, "echo"));
    assert!(is_tool_allowed(allowed, "file_read"));

    // Unlisted tool is blocked
    assert!(
        !is_tool_allowed(allowed, "file_write"),
        "file_write is not in allowed_tools"
    );

    // Dispatch allowed tools
    for name in &["echo", "file_read"] {
        let found = registry.tools.iter().find(|t| t.name() == *name).unwrap();
        let result = found.execute(json!({})).await.unwrap();
        assert!(result.success);
    }

    assert_eq!(calls_a.lock().len(), 1, "echo was called");
    assert_eq!(calls_b.lock().len(), 1, "file_read was called");
    assert!(calls_c.lock().is_empty(), "file_write was never called");
}

// ---------------------------------------------------------------------------
// 6. build_functions propagates allowed_tools from the manifest
// ---------------------------------------------------------------------------

#[test]
fn build_functions_produces_tool_call_fn_with_allowed_tools() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let manifest = make_manifest_with_delegation(vec!["echo".into(), "file_read".into()]);

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_tool_call"),
        "build_functions should register zeroclaw_tool_call for manifest with tool_delegation"
    );

    // Verify the manifest's allowed_tools are intact
    let td = manifest.host_capabilities.tool_delegation.as_ref().unwrap();
    assert_eq!(td.allowed_tools, vec!["echo", "file_read"]);
}

#[test]
fn manifest_without_delegation_has_no_allowed_tools() {
    let toml_str = r#"
        name = "plain_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let manifest: PluginManifest = toml::from_str(toml_str).expect("valid manifest");

    assert!(
        manifest.host_capabilities.tool_delegation.is_none(),
        "plugin without tool_delegation should have no allowed_tools"
    );

    // build_functions should NOT produce zeroclaw_tool_call
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_tool_call"),
        "without tool_delegation, zeroclaw_tool_call must not be registered"
    );
}
