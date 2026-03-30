//! Verify that a plugin without memory capability cannot access memory functions.
//!
//! Acceptance criterion for US-ZCL-23:
//! > Plugin without memory capability cannot access memory functions
//!
//! These tests assert that:
//! 1. A plugin with no host_capabilities section gets zero memory host functions
//! 2. A plugin with other capabilities (tool_delegation, messaging) but no memory
//!    still gets zero memory-related host functions
//! 3. A plugin with memory read/write both false gets zero memory host functions

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::PluginManifest;
use zeroclaw::security::audit::AuditLogger;

/// Minimal no-op memory backend for tests.
struct NoopMemory;

#[async_trait]
impl Memory for NoopMemory {
    fn name(&self) -> &str {
        "noop"
    }

    async fn store(
        &self,
        _key: &str,
        _content: &str,
        _category: MemoryCategory,
        _session_id: Option<&str>,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn recall(
        &self,
        _query: &str,
        _limit: usize,
        _session_id: Option<&str>,
        _since: Option<&str>,
        _until: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn get(&self, _key: &str) -> anyhow::Result<Option<MemoryEntry>> {
        Ok(None)
    }

    async fn list(
        &self,
        _category: Option<&MemoryCategory>,
        _session_id: Option<&str>,
    ) -> anyhow::Result<Vec<MemoryEntry>> {
        Ok(Vec::new())
    }

    async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
        Ok(false)
    }

    async fn count(&self) -> anyhow::Result<usize> {
        Ok(0)
    }

    async fn health_check(&self) -> bool {
        true
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

fn make_registry() -> HostFunctionRegistry {
    HostFunctionRegistry::new(Arc::new(NoopMemory), vec![], make_audit())
}

// ---------------------------------------------------------------------------
// 1. No host_capabilities at all => no memory functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_without_host_capabilities_gets_no_memory_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "bare_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "plugin with no host_capabilities must receive zero host functions, got {}",
        fns.len()
    );
}

// ---------------------------------------------------------------------------
// 2. Other host capabilities present, but no memory => no memory functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_tool_delegation_but_no_memory_gets_no_memory_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "tool_only_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.tool_delegation]
        allowed_tools = ["web_search"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let memory_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("memory")
        })
        .collect();

    assert!(
        memory_fns.is_empty(),
        "plugin with tool_delegation but no memory capability must have zero memory functions"
    );
    // Should have exactly 1 function: zeroclaw_tool_call
    assert_eq!(fns.len(), 1, "only zeroclaw_tool_call should be registered");
}

#[test]
fn plugin_with_messaging_but_no_memory_gets_no_memory_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "messaging_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["channel"]

        [host_capabilities.messaging]
        allowed_channels = ["slack"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let memory_fns: Vec<_> = fns
        .iter()
        .filter(|f| f.name().contains("memory"))
        .collect();

    assert!(
        memory_fns.is_empty(),
        "plugin with messaging but no memory capability must have zero memory functions"
    );
    assert_eq!(fns.len(), 2, "zeroclaw_send_message + zeroclaw_get_channels should be registered");
}

// ---------------------------------------------------------------------------
// 3. Memory section present but both read and write disabled => no memory fns
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_memory_both_false_gets_no_memory_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "memory_disabled_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.memory]
        read = false
        write = false
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "plugin with memory read=false write=false must receive zero host functions, got {}",
        fns.len()
    );
}

// ---------------------------------------------------------------------------
// 4. Contrast: plugin WITH memory capability DOES get memory functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_memory_read_gets_memory_read_function() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "memory_reader"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.memory]
        read = true
        write = false
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 1, "read-only memory plugin gets exactly 1 function");
}

#[test]
fn plugin_with_memory_write_gets_memory_write_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "memory_writer"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.memory]
        read = false
        write = true
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 2, "write-only memory plugin gets store + forget functions");
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));
}
