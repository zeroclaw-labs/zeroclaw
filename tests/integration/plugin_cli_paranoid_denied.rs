#![cfg(feature = "plugins-wasm")]

//! Test: Paranoid level denies all CLI execution.
//!
//! Task US-ZCL-58-1: Verifies acceptance criterion for US-ZCL-58:
//! > Paranoid level denies all CLI execution
//!
//! These tests verify that when the security level is set to `Paranoid`,
//! CLI execution is denied regardless of what the plugin manifest declares.

use std::sync::Arc;
use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::loader::NetworkSecurityLevel;
use zeroclaw::plugins::{CliCapability, PluginCapabilities, PluginManifest};
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
// Core acceptance criterion: Paranoid level denies all CLI execution
// ---------------------------------------------------------------------------

/// AC: Paranoid level denies CLI execution even when manifest declares CLI capability.
#[test]
fn paranoid_level_denies_cli_execution() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_cli_exec"),
        "Paranoid level must deny CLI execution; got functions: {:?}",
        names
    );
}

/// AC: Paranoid level denies CLI even with extensive allowlist.
#[test]
fn paranoid_level_denies_cli_even_with_allowlist() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec![
                "echo".to_string(),
                "cat".to_string(),
                "ls".to_string(),
                "git".to_string(),
            ],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();
    assert!(
        !names.contains(&"zeroclaw_cli_exec"),
        "Paranoid level must deny CLI execution regardless of allowlist"
    );
}

/// AC: Paranoid level denies CLI but allows other capabilities.
#[test]
fn paranoid_level_denies_cli_but_allows_memory() {
    use zeroclaw::plugins::MemoryCapability;

    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        memory: Some(MemoryCapability {
            read: true,
            write: true,
        }),
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    // Memory should still be allowed
    assert!(
        names.contains(&"zeroclaw_memory_recall"),
        "Memory read should be allowed in Paranoid mode"
    );
    assert!(
        names.contains(&"zeroclaw_memory_store"),
        "Memory write should be allowed in Paranoid mode"
    );

    // CLI should be denied
    assert!(
        !names.contains(&"zeroclaw_cli_exec"),
        "CLI must be denied in Paranoid mode"
    );
}

/// AC: Non-paranoid levels allow CLI when declared.
#[test]
fn non_paranoid_levels_allow_cli() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    // Default level should allow CLI
    let fns_default = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Default);
    let names_default: Vec<&str> = fns_default.iter().map(|f| f.name()).collect();
    assert!(
        names_default.contains(&"zeroclaw_cli_exec"),
        "Default level should allow CLI execution"
    );

    // Relaxed level should allow CLI
    let fns_relaxed = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Relaxed);
    let names_relaxed: Vec<&str> = fns_relaxed.iter().map(|f| f.name()).collect();
    assert!(
        names_relaxed.contains(&"zeroclaw_cli_exec"),
        "Relaxed level should allow CLI execution"
    );

    // Strict level should allow CLI (it enforces patterns, not deny outright)
    let fns_strict = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Strict);
    let names_strict: Vec<&str> = fns_strict.iter().map(|f| f.name()).collect();
    assert!(
        names_strict.contains(&"zeroclaw_cli_exec"),
        "Strict level should allow CLI execution (with pattern enforcement)"
    );
}

/// AC: Paranoid level produces zero functions when only CLI is declared.
#[test]
fn paranoid_level_produces_zero_functions_when_only_cli() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    assert!(
        fns.is_empty(),
        "Paranoid level with only CLI capability should produce zero functions"
    );
}
