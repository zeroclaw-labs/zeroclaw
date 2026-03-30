//! Test: zeroclaw_get_session_context returns current session info.
//!
//! Task US-ZCL-26-1: verify that the `context_session` host function is
//! registered when a plugin manifest declares `context.session = true`,
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
// context_session is registered when session = true
// ---------------------------------------------------------------------------

#[test]
fn context_session_registered_when_session_enabled() {
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

    assert_eq!(fns.len(), 1, "session-only context should yield 1 function");
    assert!(
        names.contains(&"context_session"),
        "expected context_session, got: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// context_session is NOT registered when session = false
// ---------------------------------------------------------------------------

#[test]
fn context_session_not_registered_when_session_disabled() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        context: Some(ContextCapability {
            session: false,
            user_identity: true,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"context_session"),
        "context_session should NOT be registered when session=false, got: {names:?}"
    );
}

// ---------------------------------------------------------------------------
// context_session is NOT registered when context capability is absent
// ---------------------------------------------------------------------------

#[test]
fn context_session_absent_when_no_context_capability() {
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
// context_session coexists with other context sub-flags
// ---------------------------------------------------------------------------

#[test]
fn context_session_coexists_with_other_context_flags() {
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
// context_session is registered alongside memory functions
// ---------------------------------------------------------------------------

#[test]
fn context_session_registered_alongside_memory() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: false,
        }),
        context: Some(ContextCapability {
            session: true,
            user_identity: false,
            agent_config: false,
        }),
        ..Default::default()
    });

    let fns = registry.build_functions(&manifest);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert_eq!(
        fns.len(),
        2,
        "memory(read) + context(session) should yield 2 functions"
    );
    assert!(names.contains(&"zeroclaw_memory_recall"));
    assert!(names.contains(&"context_session"));
}
