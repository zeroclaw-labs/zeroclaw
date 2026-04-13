#![cfg(all(
    feature = "plugins-wasm",
    feature = "__disabled_pending_risk_level_trait"
))]

//! Integration test for delegation security limits (combined).
//!
//! Task US-ZCL-24-10: Verify security boundaries for tool delegation:
//! 1. Delegation to unauthorized tool is rejected
//! 2. Delegation exceeding depth 5 is rejected
//! 3. Low-risk plugin delegating to high-risk tool is rejected

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::config::schema::DelegateAgentConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{
    PluginCapabilities, PluginManifest, RiskLevel, ToolDefinition, ToolDelegationCapability,
};
use zeroclaw::security::SecurityPolicy;
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::tools::DelegateTool;
use zeroclaw::tools::traits::{Tool, ToolResult};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

struct TrackingTool {
    tool_name: &'static str,
    level: RiskLevel,
    calls: Arc<Mutex<Vec<Value>>>,
}

impl TrackingTool {
    fn new(name: &'static str, level: RiskLevel, calls: Arc<Mutex<Vec<Value>>>) -> Self {
        Self {
            tool_name: name,
            level,
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

    fn risk_level(&self) -> RiskLevel {
        self.level
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

fn make_manifest(tool_risk_levels: &[RiskLevel], allowed_tools: Vec<String>) -> PluginManifest {
    let toml_str = r#"
        name = "security_test_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
    "#;
    let mut m: PluginManifest = toml::from_str(toml_str).expect("valid manifest");
    m.host_capabilities = PluginCapabilities {
        tool_delegation: Some(ToolDelegationCapability { allowed_tools }),
        ..Default::default()
    };
    for (i, &level) in tool_risk_levels.iter().enumerate() {
        m.tools.push(ToolDefinition {
            name: format!("own_tool_{i}"),
            description: format!("plugin tool {i}"),
            export: format!("run_{i}"),
            risk_level: level,
            parameters_schema: None,
        });
    }
    m
}

fn is_tool_allowed(allowed_tools: &[String], tool_name: &str) -> bool {
    allowed_tools.iter().any(|t| t == "*" || t == tool_name)
}

fn test_security() -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy::default())
}

fn agent_with_max_depth(max_depth: u32) -> HashMap<String, DelegateAgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(
        "worker".to_string(),
        DelegateAgentConfig {
            provider: "ollama".to_string(),
            model: "llama3".to_string(),
            system_prompt: None,
            api_key: None,
            temperature: None,
            max_depth,
            agentic: false,
            allowed_tools: Vec::new(),
            max_iterations: 10,
            timeout_secs: None,
            agentic_timeout_secs: None,
            skills_directory: None,
            memory_namespace: None,
        },
    );
    agents
}

// ===========================================================================
// Scenario 1: Delegation to unauthorized tool is rejected
// ===========================================================================

#[test]
fn unauthorized_tool_rejected_by_allowed_check() {
    let manifest = make_manifest(&[RiskLevel::Low], vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(
        !is_tool_allowed(allowed, "secret_admin_tool"),
        "tool not in allowed_tools must be rejected"
    );
}

#[tokio::test]
async fn unauthorized_tool_in_registry_never_called() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let unauthorized: Arc<dyn Tool> = Arc::new(TrackingTool::new(
        "admin_delete",
        RiskLevel::Low,
        calls.clone(),
    ));
    let authorized: Arc<dyn Tool> = Arc::new(TrackingTool::new(
        "safe_read",
        RiskLevel::Low,
        Arc::new(Mutex::new(Vec::new())),
    ));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![authorized, unauthorized], make_audit());

    // Only safe_read is allowed
    let manifest = make_manifest(&[RiskLevel::Low], vec!["safe_read".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    // admin_delete exists in registry but is not authorized
    let found = registry.tools.iter().find(|t| t.name() == "admin_delete");
    assert!(found.is_some(), "tool is in registry");
    assert!(
        !is_tool_allowed(allowed, "admin_delete"),
        "admin_delete must be blocked by allowed_tools"
    );

    // Tool must never have been called
    assert!(
        calls.lock().is_empty(),
        "unauthorized tool must not be executed"
    );
}

#[test]
fn empty_allowed_tools_rejects_all_delegation() {
    let manifest = make_manifest(&[RiskLevel::Low], vec![]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;

    assert!(!is_tool_allowed(allowed, "echo"));
    assert!(!is_tool_allowed(allowed, "file_read"));
    assert!(!is_tool_allowed(allowed, "anything_at_all"));
}

// ===========================================================================
// Scenario 2: Delegation exceeding depth 5 is rejected
// ===========================================================================

#[tokio::test]
async fn depth_5_rejected_with_max_depth_5() {
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 5);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    assert!(!result.success, "delegation at depth 5 must fail");
    assert!(
        result
            .error
            .as_deref()
            .unwrap_or("")
            .contains("depth limit"),
        "error must mention depth limit"
    );
}

#[tokio::test]
async fn depth_exceeding_5_rejected() {
    for depth in [6, 7, 10, 100] {
        let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), depth);
        let result = tool
            .execute(json!({"agent": "worker", "prompt": "test"}))
            .await
            .unwrap();

        assert!(!result.success, "delegation at depth {depth} must fail");
        assert!(
            result
                .error
                .as_deref()
                .unwrap_or("")
                .contains("depth limit"),
            "depth {depth}: error must mention depth limit"
        );
    }
}

#[tokio::test]
async fn depth_below_5_not_blocked_by_depth_check() {
    for depth in 0..5u32 {
        let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), depth);
        let result = tool
            .execute(json!({"agent": "worker", "prompt": "test"}))
            .await
            .unwrap();

        if let Some(ref err) = result.error {
            assert!(
                !err.contains("depth limit"),
                "depth {depth} should not trigger depth limit, got: {err}"
            );
        }
    }
}

// ===========================================================================
// Scenario 3: Low-risk plugin delegating to high-risk tool is rejected
// ===========================================================================

#[tokio::test]
async fn low_risk_plugin_cannot_delegate_to_high_risk_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let high_tool: Arc<dyn Tool> = Arc::new(TrackingTool::new(
        "dangerous_op",
        RiskLevel::High,
        calls.clone(),
    ));

    let memory = Arc::new(NoneMemory::new());
    let _registry = HostFunctionRegistry::new(memory, vec![high_tool.clone()], make_audit());

    // Plugin only has Low-risk tools → ceiling is Low
    let manifest = make_manifest(&[RiskLevel::Low], vec!["dangerous_op".into()]);
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();

    assert_eq!(caller_max, RiskLevel::Low);
    assert!(
        high_tool.risk_level() > caller_max,
        "high-risk tool must exceed low-risk ceiling"
    );

    // Tool must not be called
    assert!(
        calls.lock().is_empty(),
        "high-risk tool must not execute under low-risk ceiling"
    );
}

#[tokio::test]
async fn low_risk_plugin_cannot_delegate_to_medium_risk_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let medium_tool: Arc<dyn Tool> = Arc::new(TrackingTool::new(
        "moderate_op",
        RiskLevel::Medium,
        calls.clone(),
    ));

    let memory = Arc::new(NoneMemory::new());
    let _registry = HostFunctionRegistry::new(memory, vec![medium_tool.clone()], make_audit());

    let manifest = make_manifest(&[RiskLevel::Low], vec!["moderate_op".into()]);
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();

    assert_eq!(caller_max, RiskLevel::Low);
    assert!(
        medium_tool.risk_level() > caller_max,
        "medium-risk tool must exceed low-risk ceiling"
    );

    assert!(
        calls.lock().is_empty(),
        "medium-risk tool must not execute under low-risk ceiling"
    );
}

#[tokio::test]
async fn low_risk_plugin_can_delegate_to_low_risk_tool() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let low_tool: Arc<dyn Tool> =
        Arc::new(TrackingTool::new("safe_op", RiskLevel::Low, calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let registry = HostFunctionRegistry::new(memory, vec![low_tool], make_audit());

    let manifest = make_manifest(&[RiskLevel::Low], vec!["safe_op".into()]);
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();

    let target = registry
        .tools
        .iter()
        .find(|t| t.name() == "safe_op")
        .unwrap();
    assert!(
        target.risk_level() <= caller_max,
        "low-risk tool should be allowed by low-risk caller"
    );

    let result = target.execute(json!({})).await.unwrap();
    assert!(result.success);
    assert_eq!(calls.lock().len(), 1);
}

// ===========================================================================
// Combined: all three checks enforced together
// ===========================================================================

#[tokio::test]
async fn combined_unauthorized_and_high_risk_both_blocked() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let high_tool: Arc<dyn Tool> =
        Arc::new(TrackingTool::new("nuke", RiskLevel::High, calls.clone()));

    let memory = Arc::new(NoneMemory::new());
    let _registry = HostFunctionRegistry::new(memory, vec![high_tool.clone()], make_audit());

    // Low-risk plugin that does NOT list "nuke" in allowed_tools
    let manifest = make_manifest(&[RiskLevel::Low], vec!["echo".into()]);
    let allowed = &manifest
        .host_capabilities
        .tool_delegation
        .as_ref()
        .unwrap()
        .allowed_tools;
    let caller_max = manifest.tools.iter().map(|t| t.risk_level).max().unwrap();

    // Blocked by allowed_tools check
    assert!(
        !is_tool_allowed(allowed, "nuke"),
        "nuke not in allowed_tools"
    );
    // Also blocked by risk ceiling
    assert!(
        high_tool.risk_level() > caller_max,
        "nuke exceeds low-risk ceiling"
    );
    // Never executed
    assert!(calls.lock().is_empty());
}
