#![cfg(feature = "plugins-wasm")]

//! Test: Default level allows allowlist with warnings on broad patterns.
//!
//! Task US-ZCL-58-3: Verifies acceptance criterion for US-ZCL-58:
//! > Default level allows allowlist with warnings on broad patterns
//!
//! These tests verify that at Default security level:
//! 1. CLI execution is allowed when an allowlist is declared
//! 2. Broad patterns (patterns ending with '*') are permitted but trigger warnings
//! 3. Execution proceeds successfully even with broad patterns

use std::sync::Arc;

use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::loader::NetworkSecurityLevel;
use zeroclaw::plugins::{ArgPattern, CliCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::security::{validate_arguments, warn_broad_cli_patterns};

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
// Core acceptance criterion: Default level allows allowlist with warnings
// ---------------------------------------------------------------------------

/// AC: Default level allows CLI execution with allowlist.
#[test]
fn default_level_allows_cli_with_allowlist() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string(), "cat".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Default);
    let names: Vec<&str> = fns.iter().map(|f| f.name()).collect();

    assert!(
        names.contains(&"zeroclaw_cli_exec"),
        "Default level must allow CLI execution with allowlist; got functions: {:?}",
        names
    );
}

/// AC: Default level validates arguments via glob matching (unlike Strict's exact match).
#[test]
fn default_level_validates_with_glob_matching() {
    let patterns = vec![ArgPattern::new(
        "npm",
        vec!["install".to_string(), "--save*".to_string()],
    )];

    // Default level uses glob matching - --save-dev should match --save*
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "exact match 'install' should pass at Default level"
    );
    assert!(
        validate_arguments("npm", &["--save-dev"], &patterns).is_ok(),
        "glob match '--save*' should accept '--save-dev' at Default level"
    );
    assert!(
        validate_arguments("npm", &["--save-exact"], &patterns).is_ok(),
        "glob match '--save*' should accept '--save-exact' at Default level"
    );
}

/// AC: Default level allows broad patterns (patterns ending with '*').
#[test]
fn default_level_allows_broad_patterns() {
    // Broad pattern: standalone wildcard '*'
    let patterns_star = vec![ArgPattern::new("cmd", vec!["*".to_string()])];
    assert!(
        validate_arguments("cmd", &["anything"], &patterns_star).is_ok(),
        "broad pattern '*' should be allowed at Default level"
    );
    assert!(
        validate_arguments("cmd", &["--any-flag"], &patterns_star).is_ok(),
        "broad pattern '*' should match any argument at Default level"
    );

    // Broad pattern: suffix wildcard '-*'
    let patterns_suffix = vec![ArgPattern::new("git", vec!["-*".to_string()])];
    assert!(
        validate_arguments("git", &["-v"], &patterns_suffix).is_ok(),
        "broad pattern '-*' should be allowed at Default level"
    );
    assert!(
        validate_arguments("git", &["--verbose"], &patterns_suffix).is_ok(),
        "broad pattern '-*' should match '--verbose' at Default level"
    );
}

/// AC: Default level triggers warning for broad patterns but allows execution.
#[test]
fn default_level_warns_on_broad_patterns_but_allows() {
    let patterns = vec![ArgPattern::new(
        "npm",
        vec!["install".to_string(), "--save*".to_string()],
    )];

    // warn_broad_cli_patterns should run without error (it logs a warning)
    // We verify it doesn't panic and execution continues
    warn_broad_cli_patterns("test-plugin", "npm", &patterns);

    // After warning, validation should still succeed
    let result = validate_arguments("npm", &["--save-dev"], &patterns);
    assert!(
        result.is_ok(),
        "execution should proceed after warning: {:?}",
        result.err()
    );
}

/// AC: Default level warning identifies multiple broad patterns.
#[test]
fn default_level_warns_multiple_broad_patterns() {
    let patterns = vec![ArgPattern::new(
        "cmd",
        vec![
            "-*".to_string(),
            "--verbose".to_string(),
            "file*".to_string(),
            "*".to_string(),
        ],
    )];

    // Three broad patterns: "-*", "file*", "*"
    let broad = patterns[0].get_broad_patterns();
    assert_eq!(
        broad.len(),
        3,
        "should detect 3 broad patterns: {:?}",
        broad
    );
    assert!(broad.contains(&"-*"));
    assert!(broad.contains(&"file*"));
    assert!(broad.contains(&"*"));

    // Warning runs without error
    warn_broad_cli_patterns("test-plugin", "cmd", &patterns);

    // Validation still succeeds for all patterns
    assert!(validate_arguments("cmd", &["-v"], &patterns).is_ok());
    assert!(validate_arguments("cmd", &["--verbose"], &patterns).is_ok());
    assert!(validate_arguments("cmd", &["file.txt"], &patterns).is_ok());
    assert!(validate_arguments("cmd", &["anything"], &patterns).is_ok());
}

/// AC: Default level does not warn on exact (non-broad) patterns.
#[test]
fn default_level_no_warning_for_exact_patterns() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec![
            "status".to_string(),
            "log".to_string(),
            "--oneline".to_string(),
        ],
    )];

    // No broad patterns
    let broad = patterns[0].get_broad_patterns();
    assert!(
        broad.is_empty(),
        "exact patterns should not be detected as broad: {:?}",
        broad
    );

    // warn_broad_cli_patterns runs without logging (no broad patterns)
    warn_broad_cli_patterns("test-plugin", "git", &patterns);

    // Validation succeeds
    assert!(validate_arguments("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments("git", &["log"], &patterns).is_ok());
    assert!(validate_arguments("git", &["--oneline"], &patterns).is_ok());
}

// ---------------------------------------------------------------------------
// Contrast with Strict level (which rejects wildcards)
// ---------------------------------------------------------------------------

/// Contrast: Default allows wildcards that Strict rejects.
#[test]
fn default_allows_wildcards_strict_rejects() {
    use zeroclaw::security::validate_arguments_strict;

    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    // Default level: wildcard allowed
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "Default level should allow wildcard *"
    );

    // Strict level: wildcard rejected
    let strict_result = validate_arguments_strict("npm", &["install"], &patterns);
    assert!(
        strict_result.is_err(),
        "Strict level should reject wildcard *"
    );
    assert!(
        strict_result.unwrap_err().reason.contains("wildcard"),
        "Strict rejection should mention wildcard"
    );
}

/// Contrast: Default uses glob matching, Strict uses exact matching.
#[test]
fn default_glob_vs_strict_exact() {
    use zeroclaw::security::validate_arguments_strict;

    let patterns = vec![ArgPattern::new(
        "cargo",
        vec!["build".to_string(), "--release".to_string()],
    )];

    // Both levels accept exact matches
    assert!(validate_arguments("cargo", &["build"], &patterns).is_ok());
    assert!(validate_arguments_strict("cargo", &["build"], &patterns).is_ok());

    // Default uses glob - would need a pattern with wildcard to show difference
    // Both reject args not in pattern
    assert!(validate_arguments("cargo", &["test"], &patterns).is_err());
    assert!(validate_arguments_strict("cargo", &["test"], &patterns).is_err());
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Default level handles empty arguments.
#[test]
fn default_level_empty_args() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "empty args should be accepted at Default level"
    );
}

/// AC: Default level handles command with no pattern.
#[test]
fn default_level_unknown_command() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    // warn_broad_cli_patterns silently returns for unknown command
    warn_broad_cli_patterns("test-plugin", "unknown-cmd", &patterns);

    // validate_arguments fails for unknown command with args
    let result = validate_arguments("unknown-cmd", &["arg"], &patterns);
    assert!(
        result.is_err(),
        "unknown command with args should fail validation"
    );
}

/// AC: Default level rejects shell metacharacters (before glob matching).
#[test]
fn default_level_still_rejects_metacharacters() {
    let patterns = vec![ArgPattern::new(
        "echo",
        vec!["*".to_string()], // Broad pattern, would match anything
    )];

    // Even with broad pattern, shell metacharacters are rejected
    let result = validate_arguments("echo", &["hello; rm -rf /"], &patterns);
    assert!(
        result.is_err(),
        "shell metacharacters must be rejected even at Default level"
    );
    assert!(
        result.unwrap_err().reason.contains("shell metacharacter"),
        "error should mention shell metacharacter"
    );
}

/// AC: Default level allows multiple commands with different patterns.
#[test]
fn default_level_multiple_command_patterns() {
    let patterns = vec![
        ArgPattern::new("git", vec!["status".to_string(), "log*".to_string()]),
        ArgPattern::new("npm", vec!["*".to_string()]),
    ];

    // git uses specific + broad patterns
    assert!(validate_arguments("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments("git", &["log"], &patterns).is_ok());
    assert!(validate_arguments("git", &["log-all"], &patterns).is_ok());
    assert!(validate_arguments("git", &["push"], &patterns).is_err());

    // npm uses fully broad pattern
    assert!(validate_arguments("npm", &["install"], &patterns).is_ok());
    assert!(validate_arguments("npm", &["test"], &patterns).is_ok());
    assert!(validate_arguments("npm", &["run", "build"], &patterns).is_ok());
}
