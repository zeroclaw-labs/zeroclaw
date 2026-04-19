#![cfg(feature = "plugins-wasm")]

//! Unit tests for capability parsing and host function registration.
//!
//! Task US-ZCL-22-9: verify that `HostFunctionRegistry::build_functions` returns
//! the correct set of host functions based on the manifest's `host_capabilities`.
//!
//! Test cases:
//! - No capabilities → no host functions registered
//! - Memory capability only → only memory functions registered
//! - All capabilities → all functions registered
//! - Verify function count matches expected

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{
    ContextCapability, MemoryCapability, MessagingCapability, PluginCapabilities, PluginManifest,
    ToolDelegationCapability,
};
use zeroclaw::security::audit::AuditLogger;

/// Build a minimal `HostFunctionRegistry` backed by stubs.
fn make_registry() -> HostFunctionRegistry {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let memory = Arc::new(NoneMemory::new());
    let audit = Arc::new(
        AuditLogger::new(
            AuditConfig {
                enabled: false,
                ..Default::default()
            },
            tmp.path().to_path_buf(),
        )
        .expect("audit logger"),
    );
    HostFunctionRegistry::new(memory, vec![], audit)
}

/// Build a minimal `PluginManifest` with the given host capabilities.
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

// ---------------------------------------------------------------------------
// No capabilities → no host functions
// ---------------------------------------------------------------------------

#[test]
fn no_capabilities_produces_zero_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities::default());

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "no capabilities should yield 0 functions, got {}",
        fns.len()
    );
}

// ---------------------------------------------------------------------------
// Memory capability only → only memory functions
// ---------------------------------------------------------------------------

#[test]
fn memory_read_only_produces_one_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 1);
    assert_eq!(fns[0].name(), "zeroclaw_memory_recall");
}

#[test]
fn memory_write_only_produces_two_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: false,
            write: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 2);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));
}

#[test]
fn memory_read_write_produces_three_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 3);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));
}

// ---------------------------------------------------------------------------
// All capabilities → all functions registered
// ---------------------------------------------------------------------------

#[test]
fn all_capabilities_produces_all_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
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
        cli: None,
    });

    let fns = registry.build_functions(&manifest);
    // zeroclaw_memory_recall + zeroclaw_memory_store + zeroclaw_memory_forget + zeroclaw_tool_call +
    // zeroclaw_send_message + zeroclaw_get_channels + context_session + context_user_identity + context_agent_config = 9
    assert_eq!(
        fns.len(),
        9,
        "all capabilities should yield 9 functions, got {}",
        fns.len()
    );

    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));
    assert!(names.contains(&"zeroclaw_tool_call"));
    assert!(names.contains(&"zeroclaw_send_message"));
    assert!(names.contains(&"zeroclaw_get_channels"));
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}

// ---------------------------------------------------------------------------
// Partial context — only enabled sub-flags produce functions
// ---------------------------------------------------------------------------

#[test]
fn context_with_session_only_produces_one_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: true,
            user_identity: false,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 1);
    assert_eq!(fns[0].name(), "context_session");
}

// ---------------------------------------------------------------------------
// Mixed capabilities — verify exact count
// ---------------------------------------------------------------------------

#[test]
fn tool_delegation_and_messaging_only() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        tool_delegation: Some(ToolDelegationCapability {
            allowed_tools: vec![],
        }),
        messaging: Some(MessagingCapability {
            allowed_channels: vec![],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert_eq!(fns.len(), 3);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(names.contains(&"zeroclaw_tool_call"));
    assert!(names.contains(&"zeroclaw_send_message"));
    assert!(names.contains(&"zeroclaw_get_channels"));
}

// ---------------------------------------------------------------------------
// Memory capability declared but both flags false → no functions
// ---------------------------------------------------------------------------

#[test]
fn memory_capability_with_no_flags_produces_zero_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: false,
            write: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    assert!(
        fns.is_empty(),
        "memory with no flags should yield 0 functions"
    );
}
