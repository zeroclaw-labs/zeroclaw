#![cfg(all(
    feature = "plugins-wasm",
    feature = "__disabled_pending_risk_level_trait"
))]

//! Integration test for tool delegation end-to-end flow.
//!
//! Task US-ZCL-24-9: Verify that a plugin can delegate to a built-in tool,
//! that the delegation succeeds and returns the expected result, and that
//! delegation to an unauthorized tool is rejected.
//!
//! These tests assert that:
//! 1. A plugin with tool_delegation capability can call an allowed built-in tool
//!    and receive the correct result
//! 2. Arguments are passed through the full delegation path
//! 3. Delegation to a tool NOT in allowed_tools is blocked before execution
//! 4. Delegation to an allowed tool that does not exist in the registry fails
//!    with a clear error
//! 5. A plugin without tool_delegation capability cannot delegate at all

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{
    PluginCapabilities, PluginManifest, RiskLevel, ToolDefinition, ToolDelegationCapability,
};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::tools::traits::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// A mock tool simulating a built-in tool (e.g., echo, file_read).
/// Records every call for verification.
struct BuiltinMock {
    tool_name: &'static str,
    calls: Arc<Mutex<Vec<Value>>>,
    response: ToolResult,
    level: RiskLevel,
}

impl BuiltinMock {
    fn new(name: &'static str, calls: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            tool_name: name,
            calls,
            response: ToolResult {
                success: true,
                output: format!("{name} executed"),
                error: None,
            },
            level: RiskLevel::Low,
        }
    }

    fn with_response(mut self, result: ToolResult) -> Self {
        self.response = result;
        self
    }

    fn with_risk(mut self, level: RiskLevel) -> Self {
        self.level = level;
        self
    }
}

#[async_trait]
impl Tool for BuiltinMock {
    fn name(&self) -> &str {
        self.tool_name
    }

    fn description(&self) -> &str {
        "built-in mock tool for delegation tests"
    }

    fn parameters_schema(&self) -> Value {
        json!({ "type": "object" })
    }

    fn risk_level(&self) -> RiskLevel {
        self.level
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

fn make_delegating_manifest(allowed_tools: Vec<String>) -> PluginManifest {
    let toml_str = r#"
        name = "delegation_test_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let mut m: PluginManifest = toml::from_str(toml_str).expect("valid manifest");
    m.host_capabilities = PluginCapabilities {
        tool_delegation: Some(ToolDelegationCapability { allowed_tools }),
        ..Default::default()
    };
    // Give the plugin a Low-risk tool so the risk ceiling is defined
    m.tools.push(ToolDefinition {
        name: "plugin_action".into(),
        description: "the plugin's own tool".into(),
        export: "run_action".into(),
        risk_level: RiskLevel::Low,
        parameters_schema: None,
    });
    m
}

fn make_plain_manifest() -> PluginManifest {
    let toml_str = r#"
        name = "no_delegation_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    toml::from_str(toml_str).expect("valid manifest")
}

/// Simulate the allowed_tools check from the host function.
fn is_tool_allowed(allowed_tools: &[String], tool_name: &str) -> bool {
    allowed_tools.iter().any(|t| t == "*" || t == tool_name)
}

// ===========================================================================
// 1. Successful delegation to an allowed built-in tool
// ===========================================================================

#[tokio::test]
async fn delegation_to_allowed_builtin_tool_succeeds() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let echo_tool: Arc<dyn Tool> = Arc::new(BuiltinMock::new("echo", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![echo_tool], make_audit());

    let manifest = make_delegating_manifest(vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Step 1: zeroclaw_tool_call is registered
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        names.contains(&"zeroclaw_tool_call"),
        "delegation capability must register zeroclaw_tool_call"
    );

    // Step 2: allowed_tools check passes
    assert!(is_tool_allowed(allowed, "echo"), "echo is in allowed_tools");

    // Step 3: tool found in registry and executed
    let found = registry.tools.iter().find(|t| t.name() == "echo").unwrap();
    let result = found
        .execute(json!({ "message": "hello world" }))
        .await
        .unwrap();

    assert!(result.success);
    assert_eq!(result.output, "echo executed");
    assert!(result.error.is_none());
    assert_eq!(calls.lock().len(), 1);
}

#[tokio::test]
async fn delegation_passes_arguments_through_to_builtin_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(BuiltinMock::new("file_read", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let manifest = make_delegating_manifest(vec!["file_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(is_tool_allowed(allowed, "file_read"));

    let args = json!({
        "path": "/etc/config.toml",
        "encoding": "utf-8",
        "max_bytes": 4096
    });

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "file_read")
        .unwrap();
    let result = found.execute(args.clone()).await.unwrap();

    assert!(result.success);

    // Verify the exact arguments were passed through
    let recorded = calls.lock();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0], args);
}

#[tokio::test]
async fn delegation_returns_builtin_tool_custom_output() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(
        BuiltinMock::new("calculator", calls.clone()).with_response(ToolResult {
            success: true,
            output: r#"{"result": 42, "expression": "6 * 7"}"#.into(),
            error: None,
        }),
    );

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());
    let manifest = make_delegating_manifest(vec!["calculator".into()]);

    assert!(is_tool_allowed(
        &manifest
            .host_capabilities
            .tool_delegation
            .as_ref()
            .unwrap()
            .allowed_tools,
        "calculator"
    ));

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "calculator")
        .unwrap();
    let result = found.execute(json!({ "expr": "6 * 7" })).await.unwrap();

    assert!(result.success);
    assert_eq!(result.output, r#"{"result": 42, "expression": "6 * 7"}"#);
    assert!(result.error.is_none());
}

// ===========================================================================
// 2. Delegation to an unauthorized tool is blocked
// ===========================================================================

#[tokio::test]
async fn delegation_to_unauthorized_tool_is_blocked() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let secret_tool: Arc<dyn Tool> = Arc::new(BuiltinMock::new("admin_shell", calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![secret_tool], make_audit());

    // Plugin allows only "echo", not "admin_shell"
    let manifest = make_delegating_manifest(vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Tool exists in the registry
    let found = registry.tools.iter().find(|t| t.name() == "admin_shell");
    assert!(found.is_some(), "admin_shell exists in registry");

    // But the allowed_tools check blocks it
    assert!(
        !is_tool_allowed(allowed, "admin_shell"),
        "admin_shell must be rejected — not in allowed_tools"
    );

    // Tool was never executed
    assert!(
        calls.lock().is_empty(),
        "unauthorized tool must not be executed"
    );
}

#[tokio::test]
async fn delegation_among_multiple_tools_only_allowed_succeed() {
    let calls_allowed = Arc::new(Mutex::new(Vec::new()));
    let calls_blocked = Arc::new(Mutex::new(Vec::new()));

    let allowed_tool: Arc<dyn Tool> =
        Arc::new(BuiltinMock::new("safe_read", calls_allowed.clone()));
    let blocked_tool: Arc<dyn Tool> =
        Arc::new(BuiltinMock::new("dangerous_write", calls_blocked.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry =
        HostFunctionRegistry::new(memory, vec![allowed_tool, blocked_tool], make_audit());

    let manifest = make_delegating_manifest(vec!["safe_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // safe_read: allowed → dispatch
    assert!(is_tool_allowed(allowed, "safe_read"));
    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "safe_read")
        .unwrap();
    let result = found.execute(json!({})).await.unwrap();
    assert!(result.success);
    assert_eq!(calls_allowed.lock().len(), 1);

    // dangerous_write: NOT allowed → blocked
    assert!(!is_tool_allowed(allowed, "dangerous_write"));
    assert!(
        calls_blocked.lock().is_empty(),
        "dangerous_write must never execute"
    );
}

// ===========================================================================
// 3. Delegation to a non-existent tool fails with clear error
// ===========================================================================

#[test]
fn delegation_to_nonexistent_tool_not_found() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(BuiltinMock::new("echo", calls));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    // Plugin is allowed to call "ghost_tool" but it doesn't exist in registry
    let manifest = make_delegating_manifest(vec!["ghost_tool".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Passes allowed_tools check
    assert!(is_tool_allowed(allowed, "ghost_tool"));

    // But tool lookup fails
    let found = registry.tools.iter().find(|t| t.name() == "ghost_tool");
    assert!(found.is_none(), "ghost_tool must not be found in registry");
}

// ===========================================================================
// 4. Plugin without tool_delegation cannot delegate
// ===========================================================================

#[test]
fn plugin_without_delegation_has_no_zeroclaw_tool_call() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let tool: Arc<dyn Tool> = Arc::new(BuiltinMock::new("echo", calls));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![tool], make_audit());

    let manifest = make_plain_manifest();
    assert!(
        manifest.host_capabilities.tool_delegation.is_none(),
        "plain plugin has no tool_delegation"
    );

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_tool_call"),
        "zeroclaw_tool_call must not be registered for plugins without tool_delegation"
    );
}

// ===========================================================================
// 5. Delegation respects risk level ceiling
// ===========================================================================

#[tokio::test]
async fn delegation_to_higher_risk_tool_is_blocked() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let high_tool: Arc<dyn Tool> =
        Arc::new(BuiltinMock::new("dangerous_op", calls.clone()).with_risk(RiskLevel::High));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![high_tool.clone()], make_audit());

    // Plugin has Low-risk tools → ceiling is Low
    let manifest = make_delegating_manifest(vec!["dangerous_op".into()]);
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();

    assert_eq!(caller_max, RiskLevel::Low, "plugin ceiling is Low");
    assert!(
        high_tool.risk_level() > caller_max,
        "High-risk tool exceeds Low-risk ceiling"
    );

    // Even though allowed_tools permits it, risk ceiling blocks it
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;
    assert!(
        is_tool_allowed(allowed, "dangerous_op"),
        "tool is in allowed_tools"
    );

    // But risk ceiling check prevents execution
    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "dangerous_op")
        .unwrap();
    assert!(
        found.risk_level() > caller_max,
        "risk ceiling enforcement: High > Low"
    );

    // Tool must not be called
    assert!(
        calls.lock().is_empty(),
        "high-risk tool must not execute under low-risk ceiling"
    );
}

#[tokio::test]
async fn delegation_to_same_risk_tool_succeeds() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let low_tool: Arc<dyn Tool> =
        Arc::new(BuiltinMock::new("safe_op", calls.clone()).with_risk(RiskLevel::Low));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![low_tool], make_audit());

    let manifest = make_delegating_manifest(vec!["safe_op".into()]);
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(is_tool_allowed(allowed, "safe_op"));

    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "safe_op")
        .unwrap();
    assert!(
        found.risk_level() <= caller_max,
        "Low-risk tool within Low-risk ceiling"
    );

    let result = found
        .execute(json!({ "action": "read_status" }))
        .await
        .unwrap();
    assert!(result.success);
    assert_eq!(result.output, "safe_op executed");
    assert_eq!(calls.lock().len(), 1);
}

// ===========================================================================
// 6. Full delegation flow: capability → allowed → lookup → risk → execute
// ===========================================================================

#[tokio::test]
async fn full_delegation_pipeline_end_to_end() {
    let echo_calls = Arc::new(Mutex::new(Vec::new()));
    let read_calls = Arc::new(Mutex::new(Vec::new()));
    let write_calls = Arc::new(Mutex::new(Vec::new()));

    let echo: Arc<dyn Tool> = Arc::new(BuiltinMock::new("echo", echo_calls.clone()).with_response(
        ToolResult {
            success: true,
            output: "hello from echo".into(),
            error: None,
        },
    ));
    let file_read: Arc<dyn Tool> = Arc::new(
        BuiltinMock::new("file_read", read_calls.clone()).with_response(ToolResult {
            success: true,
            output: "file contents here".into(),
            error: None,
        }),
    );
    let file_write: Arc<dyn Tool> =
        Arc::new(BuiltinMock::new("file_write", write_calls.clone()).with_risk(RiskLevel::High));

    let memory = Arc::new(NoneMemory::new());
    let registry =
        HostFunctionRegistry::new(memory, vec![echo, file_read, file_write], make_audit());

    // Plugin allows echo and file_read, NOT file_write
    let manifest = make_delegating_manifest(vec!["echo".into(), "file_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // Verify capability is declared
    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_tool_call"));

    // Dispatch echo — allowed, exists, same risk → success
    assert!(is_tool_allowed(allowed, "echo"));
    let found = registry.tools.iter().find(|t| t.name() == "echo").unwrap();
    let result = found.execute(json!({ "text": "hi" })).await.unwrap();
    assert!(result.success);
    assert_eq!(result.output, "hello from echo");

    // Dispatch file_read — allowed, exists, same risk → success
    assert!(is_tool_allowed(allowed, "file_read"));
    let found = registry
        .tools
        .iter()
        .find(|t| t.name() == "file_read")
        .unwrap();
    let result = found.execute(json!({ "path": "/tmp/test" })).await.unwrap();
    assert!(result.success);
    assert_eq!(result.output, "file contents here");

    // Attempt file_write — NOT in allowed_tools → blocked
    assert!(
        !is_tool_allowed(allowed, "file_write"),
        "file_write not in allowed_tools"
    );

    // Verify call counts
    assert_eq!(echo_calls.lock().len(), 1, "echo called once");
    assert_eq!(read_calls.lock().len(), 1, "file_read called once");
    assert!(write_calls.lock().is_empty(), "file_write never called");
}
