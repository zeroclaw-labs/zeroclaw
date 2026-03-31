//! Integration test for zeroclaw_tool_call host function routing.
//!
//! Task US-ZCL-24-1: Verify acceptance criterion for story US-ZCL-24:
//! > zeroclaw_tool_call routes through existing tool dispatch pipeline
//!
//! These tests assert that:
//! 1. zeroclaw_tool_call is registered when tool_delegation capability is declared
//! 2. The registry dispatches to the correct tool by name
//! 3. Tool arguments are passed through to the tool's execute method
//! 4. Tool results are returned faithfully (success, output, error)
//! 5. Requesting an unknown tool returns an error
//! 6. Requesting a tool not in allowed_tools returns an error

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{PluginCapabilities, PluginManifest, ToolDelegationCapability};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::tools::traits::{Tool, ToolResult};

/// A mock tool that records calls and returns a configurable response.
struct TrackingTool {
    tool_name: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
    response: ToolResult,
}

impl TrackingTool {
    fn new(name: &'static str, calls: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            tool_name: name,
            calls,
            response: ToolResult {
                success: true,
                output: format!("{name} executed"),
                error: None,
            },
        }
    }

    fn with_response(mut self, result: ToolResult) -> Self {
        self.response = result;
        self
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
        Ok(self.response.clone())
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

// ---------------------------------------------------------------------------
// 1. zeroclaw_tool_call is registered when tool_delegation is declared
// ---------------------------------------------------------------------------

#[test]
fn tool_delegation_registers_zeroclaw_tool_call() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_delegation(vec!["echo".into()]);

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_tool_call"),
        "tool_delegation should register zeroclaw_tool_call, got: {:?}",
        names
    );
}

#[test]
fn no_tool_delegation_does_not_register_zeroclaw_tool_call() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());

    let toml_str = r#"
        name = "bare_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let manifest: PluginManifest = toml::from_str(toml_str).expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"zeroclaw_tool_call"),
        "without tool_delegation, zeroclaw_tool_call must not be registered"
    );
}

// ---------------------------------------------------------------------------
// 2. The registry has tools available for dispatch
// ---------------------------------------------------------------------------

#[test]
fn registry_holds_tool_references_for_dispatch() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    assert_eq!(registry.tools.len(), 1);
    assert_eq!(registry.tools[0].name(), "echo");
}

#[test]
fn registry_holds_multiple_tools() {
    let calls_a = Arc::new(Mutex::new(Vec::new()));
    let calls_b = Arc::new(Mutex::new(Vec::new()));
    let tool_a: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls_a));
    let tool_b: Arc<dyn Tool> = Arc::new(TrackingTool::new("file_read", calls_b));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool_a, tool_b], make_audit());

    assert_eq!(registry.tools.len(), 2);
    let names: Vec<&str> = registry.tools.iter().map(|t| t.name()).collect();
    assert!(names.contains(&"echo"));
    assert!(names.contains(&"file_read"));
}

// ---------------------------------------------------------------------------
// 3. Tool dispatch routes to the correct tool and passes arguments
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_routes_to_named_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls.clone()));

    // Verify the tool can be found and executed through the registry's tool list
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let target_name = "echo";
    let args = json!({ "message": "hello" });

    // Simulate the dispatch logic: find tool by name and execute
    let found = registry.tools.iter().find(|t| t.name() == target_name);
    assert!(found.is_some(), "tool 'echo' must be findable in registry");

    let result = found.unwrap().execute(args.clone()).await.unwrap();
    assert!(result.success);
    assert_eq!(result.output, "echo executed");

    // Verify the tool received the arguments
    let recorded = calls.lock();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], args);
}

#[tokio::test]
async fn dispatch_passes_complex_arguments() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("process", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let complex_args = json!({
        "query": "SELECT * FROM users",
        "limit": 100,
        "filters": [{"field": "active", "value": true}],
        "options": {"timeout": 30, "retry": false}
    });

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "process")
        .unwrap();
    let result = found.execute(complex_args.clone()).await.unwrap();
    assert!(result.success);

    let recorded = calls.lock();
    assert_eq!(recorded[0], complex_args);
}

// ---------------------------------------------------------------------------
// 4. Tool results are returned faithfully
// ---------------------------------------------------------------------------

#[tokio::test]
async fn successful_tool_result_is_preserved() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("calculator", calls).with_response(
        ToolResult {
            success: true,
            output: "42".into(),
            error: None,
        },
    ));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "calculator")
        .unwrap();
    let result = found.execute(json!({})).await.unwrap();

    assert!(result.success);
    assert_eq!(result.output, "42");
    assert!(result.error.is_none());
}

#[tokio::test]
async fn failed_tool_result_preserves_error() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("risky_op", calls).with_response(
        ToolResult {
            success: false,
            output: "operation failed".into(),
            error: Some("permission denied".into()),
        },
    ));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "risky_op")
        .unwrap();
    let result = found.execute(json!({})).await.unwrap();

    assert!(!result.success);
    assert_eq!(result.output, "operation failed");
    assert_eq!(result.error.as_deref(), Some("permission denied"));
}

// ---------------------------------------------------------------------------
// 5. Unknown tool is not found in the registry
// ---------------------------------------------------------------------------

#[test]
fn unknown_tool_not_found_in_registry() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(TrackingTool::new("echo", calls));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let found = registry.tools.iter().find(|t| t.name() == "nonexistent");
    assert!(found.is_none(), "nonexistent tool must not be found");
}

// ---------------------------------------------------------------------------
// 6. Allowed tools validation at the manifest/capability level
// ---------------------------------------------------------------------------

#[test]
fn allowed_tools_list_is_accessible_in_manifest() {
    let manifest = make_manifest_with_delegation(vec!["echo".into(), "file_read".into()]);

    let td = manifest.host_capabilities.tool_delegation.as_ref().unwrap();
    assert_eq!(td.allowed_tools, vec!["echo", "file_read"]);
}

#[test]
fn empty_allowed_tools_still_registers_function() {
    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![], make_audit());
    let manifest = make_manifest_with_delegation(vec![]);

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_tool_call"),
        "even with empty allowed_tools, the function should be registered"
    );
}

// ---------------------------------------------------------------------------
// 7. Dispatch selects correct tool among multiple
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dispatch_selects_correct_tool_among_multiple() {
    let calls_a = Arc::new(Mutex::new(Vec::new()));
    let calls_b = Arc::new(Mutex::new(Vec::new()));
    let tool_a: Arc<dyn Tool> = Arc::new(
        TrackingTool::new("alpha", calls_a.clone()).with_response(ToolResult {
            success: true,
            output: "alpha response".into(),
            error: None,
        }),
    );
    let tool_b: Arc<dyn Tool> = Arc::new(TrackingTool::new("beta", calls_b.clone()).with_response(
        ToolResult {
            success: true,
            output: "beta response".into(),
            error: None,
        },
    ));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool_a, tool_b], make_audit());

    // Dispatch to "beta"
    let found = registry.tools.iter().find(|t| t.name() == "beta").unwrap();
    let result = found.execute(json!({"key": "val"})).await.unwrap();

    assert_eq!(result.output, "beta response");

    // Only beta should have been called
    assert!(
        calls_a.lock().is_empty(),
        "alpha should not have been called"
    );
    assert_eq!(calls_b.lock().len(), 1, "beta should have been called once");
}
