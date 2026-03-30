//! Security test: memory capability denial.
//!
//! Task US-ZCL-23-10: Load a plugin WITHOUT memory capability declared.
//! Verify that calling memory host functions fails or the imports are not
//! available.
//!
//! Acceptance criterion for US-ZCL-23:
//! > Plugin without memory capability cannot access memory functions
//!
//! These tests verify the security boundary at the `HostFunctionRegistry`
//! level: a plugin that does not declare `[host_capabilities.memory]` must
//! never receive `zeroclaw_memory_store`, `zeroclaw_memory_recall`, or
//! `zeroclaw_memory_forget` host function imports — regardless of what other
//! capabilities it declares.

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{
    ContextCapability, MemoryCapability, MessagingCapability, PluginCapabilities, PluginManifest,
    ToolDelegationCapability,
};
use zeroclaw::security::audit::AuditLogger;

const MEMORY_FUNCTION_NAMES: &[&str] = &[
    "zeroclaw_memory_store",
    "zeroclaw_memory_recall",
    "zeroclaw_memory_forget",
    "memory_read",
    "memory_write",
    "memory_store",
    "memory_recall",
    "memory_forget",
];

fn make_registry() -> HostFunctionRegistry {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let memory = Arc::new(NoneMemory::new());
    let path = tmp.path().to_path_buf();
    std::mem::forget(tmp);
    let audit = Arc::new(
        AuditLogger::new(
            AuditConfig {
                enabled: false,
                ..Default::default()
            },
            path,
        )
        .expect("audit logger"),
    );
    HostFunctionRegistry::new(memory, vec![], audit)
}

fn manifest_with_caps(caps: PluginCapabilities) -> PluginManifest {
    let toml_str = r#"
[plugin]
name = "test-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

/// Assert that none of the given functions have memory-related names.
fn assert_no_memory_functions(fns: &[extism::Function], context: &str) {
    for f in fns {
        let name = f.name();
        assert!(
            !MEMORY_FUNCTION_NAMES.contains(&name),
            "[{context}] found memory host function '{name}' — \
             plugin without memory capability must not receive it"
        );
        assert!(
            !name.contains("memory"),
            "[{context}] host function '{name}' contains 'memory' — \
             possible memory function leak"
        );
    }
}

// ---------------------------------------------------------------------------
// 1. No host capabilities at all → zero memory functions
// ---------------------------------------------------------------------------

#[test]
fn no_host_capabilities_denies_all_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities::default());

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "no host_capabilities");
    assert!(fns.is_empty(), "no host capabilities should yield zero functions");
}

// ---------------------------------------------------------------------------
// 2. Only tool_delegation → no memory functions leak through
// ---------------------------------------------------------------------------

#[test]
fn tool_delegation_only_denies_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        tool_delegation: Some(ToolDelegationCapability {
            allowed_tools: vec!["web_search".into(), "calculator".into()],
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "tool_delegation only");
    assert_eq!(fns.len(), 1, "only zeroclaw_tool_call should be registered");
}

// ---------------------------------------------------------------------------
// 3. Only messaging → no memory functions leak through
// ---------------------------------------------------------------------------

#[test]
fn messaging_only_denies_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        messaging: Some(MessagingCapability {
            allowed_channels: vec!["slack".into(), "matrix".into()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "messaging only");
    assert_eq!(fns.len(), 2, "zeroclaw_send_message + zeroclaw_get_channels should be registered");
}

// ---------------------------------------------------------------------------
// 4. Only context → no memory functions leak through
// ---------------------------------------------------------------------------

#[test]
fn context_only_denies_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "context only");
    assert_eq!(fns.len(), 3, "session + user_identity + agent_config");
}

// ---------------------------------------------------------------------------
// 5. All non-memory capabilities combined → still no memory functions
// ---------------------------------------------------------------------------

#[test]
fn all_non_memory_capabilities_still_deny_memory() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: None,
        tool_delegation: Some(ToolDelegationCapability {
            allowed_tools: vec!["echo".into()],
        }),
        messaging: Some(MessagingCapability {
            allowed_channels: vec!["general".into()],
            ..Default::default()
        }),
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
    });

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "all non-memory capabilities");
    // zeroclaw_tool_call + zeroclaw_send_message + zeroclaw_get_channels + 3 context = 6
    assert_eq!(fns.len(), 6, "6 non-memory functions expected");
}

// ---------------------------------------------------------------------------
// 6. Memory capability present but both flags disabled → denial
// ---------------------------------------------------------------------------

#[test]
fn memory_capability_both_flags_false_denies_all_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: false,
            write: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "memory read=false write=false");
    assert!(fns.is_empty());
}

// ---------------------------------------------------------------------------
// 7. TOML manifest without host_capabilities section → denial
// ---------------------------------------------------------------------------

#[test]
fn manifest_toml_without_host_capabilities_denies_memory() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "no-host-caps"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "no host_capabilities in TOML");
    assert!(fns.is_empty());
}

// ---------------------------------------------------------------------------
// 8. TOML manifest with non-memory host capabilities → denial
// ---------------------------------------------------------------------------

#[test]
fn manifest_toml_with_tool_delegation_only_denies_memory() {
    let registry = make_registry();
    let manifest: PluginManifest = toml::from_str(
        r#"
        name = "tool-delegator"
        version = "0.1.0"
        wasm_path = "plugin.wasm"
        capabilities = ["tool"]

        [host_capabilities.tool_delegation]
        allowed_tools = ["web_search"]
        "#,
    )
    .expect("valid manifest");

    let fns = registry.build_functions(&manifest);
    assert_no_memory_functions(&fns, "TOML tool_delegation only");
}

// ---------------------------------------------------------------------------
// 9. Contrast: with memory read+write, functions ARE present
// ---------------------------------------------------------------------------

#[test]
fn memory_capability_enabled_does_produce_memory_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        names.iter().any(|n| n.contains("memory")),
        "contrast: plugin WITH memory capability must receive memory functions"
    );
    assert_eq!(fns.len(), 3, "recall + store + forget");
}

// ---------------------------------------------------------------------------
// 10. Memory read-only denies store and forget
// ---------------------------------------------------------------------------

#[test]
fn memory_read_only_denies_write_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_memory_store"),
        "read-only plugin must not receive zeroclaw_memory_store"
    );
    assert!(
        !names.contains(&"zeroclaw_memory_forget"),
        "read-only plugin must not receive zeroclaw_memory_forget"
    );
    assert_eq!(fns.len(), 1, "only recall function expected");
}

// ---------------------------------------------------------------------------
// 11. Memory write-only denies recall
// ---------------------------------------------------------------------------

#[test]
fn memory_write_only_denies_recall_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: false,
            write: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_memory_recall"),
        "write-only plugin must not receive zeroclaw_memory_recall"
    );
    assert!(
        !names.contains(&"memory_read"),
        "write-only plugin must not receive memory_read"
    );
    assert_eq!(fns.len(), 2, "only store + forget expected");
}
