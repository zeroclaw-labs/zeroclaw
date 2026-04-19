#![cfg(feature = "plugins-wasm")]

//! Test: Context is read-only with no mutation capability.
//!
//! Task US-ZCL-26-4: verify that the context capability exposes only read-only
//! getter host functions and does NOT provide any mutation (set/update/write)
//! functions. This ensures plugins cannot tamper with session context, user
//! identity, or agent configuration.

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::{ContextCapability, PluginCapabilities, PluginManifest};
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
name = "test-context-readonly"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

/// All known context host function names (read-only getters).
const CONTEXT_READ_FNS: &[&str] = &[
    "context_session",
    "context_user_identity",
    "context_agent_config",
];

/// Patterns that would indicate a mutation host function.
const MUTATION_PATTERNS: &[&str] = &[
    "context_set",
    "context_update",
    "context_write",
    "context_delete",
    "context_put",
    "context_modify",
    "context_mutate",
    "set_session",
    "set_user_identity",
    "set_agent_config",
    "update_session",
    "update_user_identity",
    "update_agent_config",
];

// ---------------------------------------------------------------------------
// All context flags enabled produces only read-only functions
// ---------------------------------------------------------------------------

#[test]
fn context_all_enabled_produces_only_read_functions() {
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
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Should produce exactly the 3 read-only getters
    assert_eq!(
        fns.len(),
        3,
        "all context flags should yield exactly 3 read-only functions"
    );
    for expected in CONTEXT_READ_FNS {
        assert!(
            names.contains(expected),
            "expected read-only function '{expected}', got: {names:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// No mutation functions are registered even with all flags enabled
// ---------------------------------------------------------------------------

#[test]
fn context_no_mutation_functions_registered() {
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
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    for pattern in MUTATION_PATTERNS {
        assert!(
            !names.iter().any(|n| n.contains(pattern)),
            "found mutation function matching '{pattern}' in context host functions: {names:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// ContextCapability struct has no write/mutation fields
// ---------------------------------------------------------------------------

#[test]
fn context_capability_has_only_read_fields() {
    // ContextCapability should only have boolean read flags.
    // If a `write` or `mutation` field were added, this test would fail
    // because the struct literal below would be incomplete.
    let cap = ContextCapability {
        session: true,
        user_identity: true,
        agent_config: true,
    };

    // Verify all fields are just booleans controlling read access
    assert!(cap.session);
    assert!(cap.user_identity);
    assert!(cap.agent_config);
}

// ---------------------------------------------------------------------------
// Context with all capabilities does NOT include any memory-write functions
// ---------------------------------------------------------------------------

#[test]
fn context_does_not_grant_memory_write_functions() {
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
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Context capability must not leak memory-write or tool-call functions
    assert!(
        !names.contains(&"zeroclaw_memory_store"),
        "context capability must not grant zeroclaw_memory_store"
    );
    assert!(
        !names.contains(&"zeroclaw_memory_forget"),
        "context capability must not grant zeroclaw_memory_forget"
    );
    assert!(
        !names.contains(&"zeroclaw_tool_call"),
        "context capability must not grant zeroclaw_tool_call"
    );
    assert!(
        !names.contains(&"zeroclaw_send_message"),
        "context capability must not grant zeroclaw_send_message"
    );
}

// ---------------------------------------------------------------------------
// Context functions are isolated — enabling context does not affect other caps
// ---------------------------------------------------------------------------

#[test]
fn context_functions_isolated_from_other_capabilities() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Memory functions: recall (read) + store + forget (write) = 3
    // Context functions: session + user_identity + agent_config = 3
    // Total = 6
    assert_eq!(
        fns.len(),
        6,
        "memory(rw) + context(all) should yield 6 functions"
    );

    // Context functions are strictly read-only getters
    for ctx_fn in CONTEXT_READ_FNS {
        assert!(names.contains(ctx_fn), "missing context function: {ctx_fn}");
    }

    // Memory write functions exist only because memory.write=true, not context
    assert!(names.contains(&"zeroclaw_memory_store"));
    assert!(names.contains(&"zeroclaw_memory_forget"));
    assert!(names.contains(&"zeroclaw_memory_recall"));
}
