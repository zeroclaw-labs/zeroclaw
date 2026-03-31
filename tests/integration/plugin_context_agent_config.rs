//! Test: zeroclaw_get_agent_config returns agent personality/identity config.
//!
//! Task US-ZCL-26-3: verify that the `context_agent_config` host function is
//! registered when a plugin manifest declares `context.agent_config = true`,
//! and that it is NOT registered when the flag is absent or false.

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
name = "test-context-plugin"
version = "0.1.0"
wasm_path = "plugin.wasm"
capabilities = ["tool"]
"#;
    let mut m = PluginManifest::parse(toml_str).unwrap();
    m.host_capabilities = caps;
    m
}

// ---------------------------------------------------------------------------
// context_agent_config is registered when agent_config = true
// ---------------------------------------------------------------------------

#[test]
fn context_agent_config_registered_when_enabled() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: false,
            user_identity: false,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        1,
        "agent_config-only context should yield 1 function"
    );
    assert!(
        names.contains(&"context_agent_config"),
        "expected context_agent_config, got: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// context_agent_config is NOT registered when agent_config = false
// ---------------------------------------------------------------------------

#[test]
fn context_agent_config_not_registered_when_disabled() {
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
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"context_agent_config"),
        "context_agent_config should NOT be registered when agent_config=false, got: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// context_agent_config is NOT registered when context capability is absent
// ---------------------------------------------------------------------------

#[test]
fn context_agent_config_absent_when_no_context_capability() {
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
// context_agent_config coexists with other context sub-flags
// ---------------------------------------------------------------------------

#[test]
fn context_agent_config_coexists_with_other_context_flags() {
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

    assert_eq!(fns.len(), 3, "all context flags should yield 3 functions");
    assert!(names.contains(&"context_session"));
    assert!(names.contains(&"context_user_identity"));
    assert!(names.contains(&"context_agent_config"));
}

// ---------------------------------------------------------------------------
// context_agent_config is registered alongside memory functions
// ---------------------------------------------------------------------------

#[test]
fn context_agent_config_registered_alongside_memory() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        context: Some(ContextCapability {
            session: false,
            user_identity: false,
            agent_config: true,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        2,
        "memory(read) + context(agent_config) should yield 2 functions"
    );
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"context_agent_config"));
}
