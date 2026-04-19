#![cfg(feature = "plugins-wasm")]

//! Test: Relaxed level requires allowlist (no wildcards ever).
//!
//! Task US-ZCL-58-4: Verifies acceptance criterion for US-ZCL-58:
//! > Relaxed level requires allowlist (no wildcards ever)
//!
//! These tests verify that at Relaxed security level:
//! 1. CLI execution requires an allowlist (allowed_commands must be declared)
//! 2. Wildcards in command allowlists are NEVER permitted (same as all other levels)
//! 3. Valid allowlists enable CLI execution without warnings (unlike Default)

use std::sync::Arc;

use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::loader::NetworkSecurityLevel;
use zeroclaw::plugins::{ArgPattern, CliCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::security::{validate_arguments, validate_command_allowlist};

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
// Core acceptance criterion: Relaxed level requires allowlist
// ---------------------------------------------------------------------------

/// AC: Relaxed level allows CLI execution only with an explicit allowlist.
#[test]
fn relaxed_level_requires_allowlist_for_cli() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string(), "ls".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Relaxed);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_cli_exec"),
        "Relaxed level must allow CLI execution with allowlist; got functions: {:?}",
        names
    );
}

/// AC: Relaxed level does NOT expose CLI function without CLI capability.
#[test]
fn relaxed_level_no_cli_without_capability() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: None, // No CLI capability declared
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Relaxed);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        !names.contains(&"zeroclaw_cli_exec"),
        "Relaxed level must not expose CLI without capability; got functions: {:?}",
        names
    );
}

// ---------------------------------------------------------------------------
// Core acceptance criterion: No wildcards ever (at Relaxed level)
// ---------------------------------------------------------------------------

/// AC: Wildcards in command allowlist are rejected at Relaxed level.
/// This is the key security invariant - wildcards are NEVER allowed in command allowlists.
#[test]
fn relaxed_level_rejects_wildcard_in_allowlist() {
    let allowed = vec!["*".to_string()];
    let result = validate_command_allowlist("ls", &allowed);

    assert!(
        result.is_err(),
        "wildcard must be rejected at Relaxed level, got success with path: {:?}",
        result.ok()
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("wildcard"),
        "error should mention wildcards: {}",
        err.reason
    );
}

/// AC: Wildcards mixed with valid commands are rejected at Relaxed level.
#[test]
fn relaxed_level_rejects_wildcard_mixed_with_valid() {
    let allowed = vec![
        "ls".to_string(),
        "*".to_string(), // Wildcard hidden among valid commands
        "echo".to_string(),
    ];
    let result = validate_command_allowlist("ls", &allowed);

    assert!(
        result.is_err(),
        "wildcard mixed with valid commands must be rejected at Relaxed level"
    );
}

/// AC: Whitespace-padded wildcards are rejected at Relaxed level.
#[test]
fn relaxed_level_rejects_whitespace_wildcard() {
    let test_cases = [
        " *".to_string(),
        "* ".to_string(),
        " * ".to_string(),
        "\t*".to_string(),
    ];

    for allowed in test_cases {
        let result = validate_command_allowlist("ls", std::slice::from_ref(&allowed));
        assert!(
            result.is_err(),
            "whitespace-padded wildcard '{}' must be rejected at Relaxed level",
            allowed.escape_debug()
        );
    }
}

// ---------------------------------------------------------------------------
// Contrast: Relaxed vs other levels
// ---------------------------------------------------------------------------

/// Relaxed level uses glob matching for arguments (same as Default).
#[test]
fn relaxed_level_uses_glob_matching_for_args() {
    let patterns = vec![ArgPattern::new(
        "npm",
        vec!["install".to_string(), "--save*".to_string()],
    )];

    // Glob patterns work at Relaxed level
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "exact match 'install' should pass at Relaxed level"
    );
    assert!(
        validate_arguments("npm", &["--save-dev"], &patterns).is_ok(),
        "glob match '--save*' should accept '--save-dev' at Relaxed level"
    );
    assert!(
        validate_arguments("npm", &["--save-exact"], &patterns).is_ok(),
        "glob match '--save*' should accept '--save-exact' at Relaxed level"
    );
}

/// Relaxed level allows broad argument patterns (like Default, unlike Strict).
#[test]
fn relaxed_level_allows_broad_arg_patterns() {
    // Broad pattern: standalone wildcard '*' in args
    let patterns_star = vec![ArgPattern::new("cmd", vec!["*".to_string()])];
    assert!(
        validate_arguments("cmd", &["anything"], &patterns_star).is_ok(),
        "broad arg pattern '*' should be allowed at Relaxed level"
    );
    assert!(
        validate_arguments("cmd", &["--any-flag"], &patterns_star).is_ok(),
        "broad arg pattern '*' should match any argument at Relaxed level"
    );

    // This is arg pattern wildcard (allowed), not command allowlist wildcard (rejected)
}

/// Key difference from command allowlist: arg patterns CAN use wildcards.
#[test]
fn relaxed_arg_wildcards_allowed_but_command_wildcards_rejected() {
    // COMMAND allowlist wildcards: ALWAYS rejected
    let cmd_result = validate_command_allowlist("ls", &["*".to_string()]);
    assert!(
        cmd_result.is_err(),
        "command allowlist wildcard must be rejected"
    );

    // ARG pattern wildcards: allowed at Relaxed/Default (rejected only at Strict)
    let arg_patterns = vec![ArgPattern::new("git", vec!["*".to_string()])];
    let arg_result = validate_arguments("git", &["status"], &arg_patterns);
    assert!(
        arg_result.is_ok(),
        "arg pattern wildcard is allowed at Relaxed level: {:?}",
        arg_result.err()
    );
}

// ---------------------------------------------------------------------------
// Explicit allowlist behavior
// ---------------------------------------------------------------------------

/// AC: Relaxed level validates commands against the explicit allowlist.
#[test]
fn relaxed_level_validates_against_allowlist() {
    let allowed = vec!["ls".to_string(), "echo".to_string()];

    // Allowed command passes
    let result = validate_command_allowlist("ls", &allowed);
    assert!(
        result.is_ok(),
        "allowed command 'ls' should pass: {:?}",
        result.err()
    );

    // Non-allowed command fails
    let result = validate_command_allowlist("rm", &allowed);
    assert!(result.is_err(), "non-allowed command 'rm' should fail");

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("not in allowlist"),
        "error should mention 'not in allowlist': {}",
        err.reason
    );
}

/// AC: Relaxed level requires non-empty allowlist for command validation.
#[test]
fn relaxed_level_empty_allowlist_rejects_all() {
    let allowed: Vec<String> = vec![];

    // Any command fails with empty allowlist
    let result = validate_command_allowlist("ls", &allowed);
    assert!(
        result.is_err(),
        "empty allowlist should reject all commands"
    );
}

// ---------------------------------------------------------------------------
// Security level independence for wildcard rejection
// ---------------------------------------------------------------------------

/// AC: Wildcard rejection in command allowlist is unconditional.
/// This test demonstrates that the rejection is identical across levels.
#[test]
fn wildcard_rejection_identical_across_levels() {
    // validate_command_allowlist doesn't take a security level parameter
    // because the behavior is unconditional. We call it multiple times
    // to demonstrate consistency.
    let wildcard_lists: Vec<Vec<String>> = vec![
        vec!["*".to_string()],
        vec!["ls".to_string(), "*".to_string()],
        vec![" * ".to_string()],
    ];

    for allowed in &wildcard_lists {
        let result = validate_command_allowlist("ls", allowed);
        assert!(
            result.is_err(),
            "wildcard must be rejected for allowlist {:?}",
            allowed
        );

        let err = result.unwrap_err();
        assert!(
            err.reason.contains("wildcard"),
            "rejection reason must mention wildcard: {}",
            err.reason
        );
    }
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Relaxed level handles empty arguments.
#[test]
fn relaxed_level_empty_args() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "empty args should be accepted at Relaxed level"
    );
}

/// AC: Relaxed level still rejects shell metacharacters.
#[test]
fn relaxed_level_still_rejects_metacharacters() {
    let patterns = vec![ArgPattern::new(
        "echo",
        vec!["*".to_string()], // Broad arg pattern
    )];

    // Even with broad arg pattern, shell metacharacters are rejected
    let result = validate_arguments("echo", &["hello; rm -rf /"], &patterns);
    assert!(
        result.is_err(),
        "shell metacharacters must be rejected even at Relaxed level"
    );
    assert!(
        result.unwrap_err().reason.contains("shell metacharacter"),
        "error should mention shell metacharacter"
    );
}

/// AC: Glob patterns in command allowlist are not bare wildcards.
/// Commands like "git*" are NOT treated as bare wildcard rejection.
#[test]
fn relaxed_level_glob_command_patterns_not_bare_wildcard() {
    // These contain '*' but are NOT bare wildcards
    let patterns = ["ls*", "git-*", "*-helper"];

    for pattern in patterns {
        let result = validate_command_allowlist("ls", &[pattern.to_string()]);

        // These fail for "not found" or "not in allowlist", NOT for "wildcards"
        if let Err(err) = result {
            // Error should NOT be about wildcards
            assert!(
                !err.reason.contains("Wildcards"),
                "glob pattern '{}' should not trigger wildcard rejection, error: {}",
                pattern,
                err.reason
            );
        }
    }
}
