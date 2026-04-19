#![cfg(feature = "plugins-wasm")]

//! Verify that a plugin without messaging capability cannot access messaging functions.
//!
//! Task US-ZCL-25-5: Acceptance criterion for story US-ZCL-25:
//! > Plugin without messaging capability cannot access messaging functions
//!
//! These tests assert that:
//! 1. A plugin with no host_capabilities gets zero messaging host functions
//! 2. A plugin with other capabilities (tool_delegation, memory) but no messaging
//!    still gets zero messaging-related host functions
//! 3. Contrast: a plugin WITH messaging capability DOES get zeroclaw_send_message

use async_trait::async_trait;
use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::traits::{Memory, MemoryCategory, MemoryEntry};
use zeroclaw::plugins::PluginManifest;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
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
// 1. No host_capabilities at all => no messaging functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_without_host_capabilities_gets_no_messaging_functions() {
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
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("channel") || name.contains("send_message")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin with no host_capabilities must have zero messaging functions"
    );
}

// ---------------------------------------------------------------------------
// 2. Only tool_delegation capability => no messaging functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_tool_delegation_but_no_messaging_gets_no_messaging_functions() {
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
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("channel") || name.contains("send_message")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin with tool_delegation but no messaging must have zero messaging functions"
    );
    // Should have exactly 1 function: zeroclaw_tool_call
    assert_eq!(fns.len(), 1, "only zeroclaw_tool_call should be registered");
}

// ---------------------------------------------------------------------------
// 3. Only memory capability => no messaging functions
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_memory_but_no_messaging_gets_no_messaging_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "memory_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.memory]
        read = true
        write = true
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("channel") || name.contains("send_message")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin with memory but no messaging must have zero messaging functions"
    );
    // Should have memory functions but no messaging
    assert!(!fns.is_empty(), "should have memory functions");
}

// ---------------------------------------------------------------------------
// 4. Both tool_delegation and memory, but no messaging => no messaging fns
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_all_non_messaging_capabilities_gets_no_messaging_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "everything_but_messaging"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.tool_delegation]
        allowed_tools = ["web_search"]

        [host_capabilities.memory]
        read = true
        write = true
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("channel") || name.contains("send_message")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "plugin with tool_delegation + memory but no messaging must have zero messaging functions"
    );
    assert!(
        fns.len() >= 2,
        "should have tool_delegation + memory functions, got {}",
        fns.len()
    );
}

// ---------------------------------------------------------------------------
// 5. TOML manifest without [host_capabilities.messaging] section
// ---------------------------------------------------------------------------

#[test]
fn toml_manifest_without_messaging_section_gets_no_messaging_functions() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "no_messaging_section"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["channel"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let messaging_fns: Vec<_> = fns
        .iter()
        .filter(|f| {
            let name = f.name();
            name.contains("channel") || name.contains("send_message")
        })
        .collect();

    assert!(
        messaging_fns.is_empty(),
        "TOML manifest without messaging section must have zero messaging functions"
    );
}

// ---------------------------------------------------------------------------
// 6. Contrast: plugin WITH messaging capability DOES get zeroclaw_send_message
// ---------------------------------------------------------------------------

#[test]
fn plugin_with_messaging_capability_gets_zeroclaw_send_message() {
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
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_send_message"),
        "plugin WITH messaging capability must get zeroclaw_send_message, got: {:?}",
        names
    );
}

#[test]
fn plugin_with_messaging_and_other_capabilities_gets_zeroclaw_send_message() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "full_plugin"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool", "channel"]

        [host_capabilities.tool_delegation]
        allowed_tools = ["web_search"]

        [host_capabilities.memory]
        read = true
        write = false

        [host_capabilities.messaging]
        allowed_channels = ["slack", "telegram"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_send_message"),
        "plugin with messaging + other capabilities must get zeroclaw_send_message, got: {:?}",
        names
    );
    // Should also have tool_delegation and memory functions
    assert!(
        fns.len() >= 3,
        "should have zeroclaw_send_message + tool_call + memory_recall, got {}",
        fns.len()
    );
}
