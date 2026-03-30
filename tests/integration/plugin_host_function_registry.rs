//! Verify that HostFunctionRegistry accepts references to ZeroClaw subsystems.
//!
//! Acceptance criterion for US-ZCL-22:
//! > HostFunctionRegistry struct accepts references to ZeroClaw subsystems

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::security::audit::AuditLogger;

#[test]
fn host_function_registry_accepts_subsystem_references() {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");

    let memory = Arc::new(NoneMemory::new());

    let audit_config = AuditConfig {
        enabled: false,
        ..Default::default()
    };
    let audit = Arc::new(
        AuditLogger::new(audit_config, tmp.path().to_path_buf())
            .expect("failed to create AuditLogger"),
    );

    let registry = HostFunctionRegistry::new(memory.clone(), vec![], audit.clone());

    // The registry should hold the same Arc references we passed in.
    assert!(Arc::ptr_eq(&registry.memory, &(memory as Arc<dyn zeroclaw::memory::traits::Memory>)));
    assert!(registry.tools.is_empty());
    assert!(Arc::ptr_eq(&registry.audit, &audit));
}

#[test]
fn host_function_registry_accepts_multiple_tools() {
    use async_trait::async_trait;
    use zeroclaw::tools::traits::{Tool, ToolResult};

    struct StubTool(&'static str);

    #[async_trait]
    impl Tool for StubTool {
        fn name(&self) -> &str {
            self.0
        }
        fn description(&self) -> &str {
            "stub"
        }
        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({})
        }
        async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
            Ok(ToolResult {
                success: true,
                output: String::new(),
                error: None,
            })
        }
    }

    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let memory = Arc::new(NoneMemory::new());
    let audit = Arc::new(
        AuditLogger::new(
            AuditConfig {
                enabled: false,
                ..Default::default()
            },
            tmp.path().to_path_buf(),
        )
        .expect("failed to create AuditLogger"),
    );

    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(StubTool("tool_a")),
        Arc::new(StubTool("tool_b")),
    ];

    let registry = HostFunctionRegistry::new(memory, tools, audit);

    assert_eq!(registry.tools.len(), 2);
    assert_eq!(registry.tools[0].name(), "tool_a");
    assert_eq!(registry.tools[1].name(), "tool_b");
}
