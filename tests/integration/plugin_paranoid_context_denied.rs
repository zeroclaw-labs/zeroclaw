//! Test: Paranoid mode denies all context access.
//!
//! Task US-ZCL-26-5: verify that when the security level is `Paranoid`, no
//! context host functions are registered even if the manifest declares all
//! context sub-capabilities (`session`, `user_identity`, `agent_config`).

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::loader::NetworkSecurityLevel;
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
name = "test-paranoid-context"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

/// All three context flags enabled — helper for multiple tests.
fn all_context_caps() -> PluginCapabilities {
    PluginCapabilities {
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    }
}

// ---------------------------------------------------------------------------
// Paranoid mode: all context functions denied even when manifest enables them
// ---------------------------------------------------------------------------

#[test]
fn paranoid_denies_all_context_functions() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"context_session"),
        "paranoid mode must deny context_session, got: {names:?}"
    );
    assert!(
        !names.contains(&"context_user_identity"),
        "paranoid mode must deny context_user_identity, got: {names:?}"
    );
    assert!(
        !names.contains(&"context_agent_config"),
        "paranoid mode must deny context_agent_config, got: {names:?}"
    );
    assert!(
        fns.is_empty(),
        "paranoid mode should yield 0 functions for context-only plugin, got {}",
        fns.len()
    );
}

// ---------------------------------------------------------------------------
// Paranoid mode: context denied even when mixed with other capabilities
// ---------------------------------------------------------------------------

#[test]
fn paranoid_denies_context_but_allows_memory() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        context: Some(ContextCapability {
            session: true,
            user_identity: true,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Memory should still be registered
    assert!(
        names.contains(&"zeroclaw_memory_recall"),
        "memory functions should still be available in paranoid mode, got: {names:?}"
    );

    // Context must be denied
    assert!(
        !names.contains(&"context_session"),
        "paranoid mode must deny context_session even when memory is allowed, got: {names:?}"
    );
    assert!(
        !names.contains(&"context_user_identity"),
        "paranoid mode must deny context_user_identity, got: {names:?}"
    );
    assert!(
        !names.contains(&"context_agent_config"),
        "paranoid mode must deny context_agent_config, got: {names:?}"
    );

    assert_eq!(
        fns.len(),
        1,
        "only memory(read) should yield 1 function in paranoid mode"
    );
}

// ---------------------------------------------------------------------------
// Non-paranoid levels still allow context access
// ---------------------------------------------------------------------------

#[test]
fn default_level_allows_context() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Default);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        3,
        "default mode should yield 3 context functions"
    );
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}

#[test]
fn strict_level_allows_context() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Strict);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(fns.len(), 3, "strict mode should yield 3 context functions");
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}

#[test]
fn relaxed_level_allows_context() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Relaxed);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        3,
        "relaxed mode should yield 3 context functions"
    );
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}

// ---------------------------------------------------------------------------
// build_functions (no explicit level) still defaults to allowing context
// ---------------------------------------------------------------------------

#[test]
fn build_functions_default_allows_context() {
    let registry = make_registry();
    let manifest = manifest_with_caps(all_context_caps());

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        3,
        "build_functions (default) should yield 3 context functions"
    );
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}
