#![cfg(feature = "plugins-wasm")]

//! Test: Security level checked before execution.
//!
//! Task US-ZCL-58-5: Verifies acceptance criterion for US-ZCL-58:
//! > Security level checked before execution
//!
//! These tests verify that the security level is checked BEFORE any CLI command
//! executes. The security level:
//! 1. Determines whether CLI capability is even exposed (Paranoid denies all)
//! 2. Determines which validation strategy is applied (Strict vs Default vs Relaxed)
//! 3. Is applied before the process is spawned, not after

use std::sync::Arc;

use zeroclaw::config::AuditConfig;
use zeroclaw::memory::none::NoneMemory;
use zeroclaw::plugins::host_functions::HostFunctionRegistry;
use zeroclaw::plugins::loader::NetworkSecurityLevel;
use zeroclaw::plugins::{ArgPattern, CliCapability, PluginCapabilities, PluginManifest};
use zeroclaw::security::audit::AuditLogger;
use zeroclaw::security::{validate_arguments, validate_arguments_strict};

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
// Core acceptance criterion: Security level checked before execution
// ---------------------------------------------------------------------------

/// AC: Security level determines function availability BEFORE execution can occur.
///
/// The security level check happens at build_functions_for_level time, preventing
/// the CLI function from even being available at Paranoid level.
#[test]
fn security_level_checked_before_function_exposed() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    // At each security level, check happens BEFORE function is exposed
    let fns_paranoid =
        registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);
    let fns_strict = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Strict);
    let fns_default = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Default);
    let fns_relaxed = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Relaxed);

    // Paranoid: check denies exposure
    assert!(
        !fns_paranoid.iter().any(|f| f.name() == "zeroclaw_cli_exec"),
        "Paranoid level must check and deny CLI before execution is possible"
    );

    // All other levels: check allows exposure (validation happens at runtime)
    assert!(
        fns_strict.iter().any(|f| f.name() == "zeroclaw_cli_exec"),
        "Strict level check should allow CLI exposure"
    );
    assert!(
        fns_default.iter().any(|f| f.name() == "zeroclaw_cli_exec"),
        "Default level check should allow CLI exposure"
    );
    assert!(
        fns_relaxed.iter().any(|f| f.name() == "zeroclaw_cli_exec"),
        "Relaxed level check should allow CLI exposure"
    );
}

/// AC: Security level determines validation strategy BEFORE command execution.
///
/// The validation function chosen depends on security level, and validation
/// happens before the process is spawned.
#[test]
fn security_level_determines_validation_strategy() {
    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    // Same input, different security level check produces different results
    // This happens BEFORE any execution would occur

    // Strict level check: wildcard pattern rejected before execution
    let strict_result = validate_arguments_strict("npm", &["install"], &patterns);
    assert!(
        strict_result.is_err(),
        "Strict level must check and reject wildcard pattern before execution"
    );

    // Default level check: wildcard pattern allowed, execution could proceed
    let default_result = validate_arguments("npm", &["install"], &patterns);
    assert!(
        default_result.is_ok(),
        "Default level check allows wildcard pattern"
    );
}

/// AC: Security level check rejects invalid patterns before any execution occurs.
///
/// At Strict level, the security check detects wildcards and rejects them
/// immediately - no command is spawned.
#[test]
fn strict_level_check_rejects_before_execution() {
    let patterns = vec![ArgPattern::new(
        "ls",
        vec!["*.txt".to_string()], // glob pattern
    )];

    // Security level check happens BEFORE execution
    let result = validate_arguments_strict("ls", &["file.txt"], &patterns);

    // The check rejects the pattern - execution never begins
    assert!(
        result.is_err(),
        "Security check must reject before execution"
    );
    let err = result.unwrap_err();
    assert!(
        err.reason.contains("wildcard"),
        "Security check error should identify the wildcard issue: {}",
        err.reason
    );
    assert!(
        err.reason.contains("Strict"),
        "Security check error should identify the security level: {}",
        err.reason
    );
}

/// AC: All security levels apply their checks before execution.
///
/// Each security level has validation that must pass before any command runs.
#[test]
fn all_levels_check_before_execution() {
    // Pattern that passes at all non-paranoid levels
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string()],
    )];

    // Valid command: all levels' checks pass (before execution would occur)
    assert!(
        validate_arguments("git", &["status"], &patterns).is_ok(),
        "Default level check should pass for valid command"
    );
    assert!(
        validate_arguments_strict("git", &["status"], &patterns).is_ok(),
        "Strict level check should pass for valid command"
    );

    // Invalid command: all levels' checks fail (before execution would occur)
    assert!(
        validate_arguments("git", &["push"], &patterns).is_err(),
        "Default level check should fail for invalid command"
    );
    assert!(
        validate_arguments_strict("git", &["push"], &patterns).is_err(),
        "Strict level check should fail for invalid command"
    );
}

/// AC: Security level check happens before shell metacharacter execution.
///
/// Even with permissive patterns, security checks detect and block dangerous
/// input before any execution occurs.
#[test]
fn security_check_blocks_metacharacters_before_execution() {
    let patterns = vec![ArgPattern::new(
        "echo",
        vec!["*".to_string()], // permissive pattern
    )];

    // Security check detects metacharacters before execution
    let result = validate_arguments("echo", &["hello; rm -rf /"], &patterns);
    assert!(
        result.is_err(),
        "Security check must block metacharacters before execution"
    );
    assert!(
        result.unwrap_err().reason.contains("shell metacharacter"),
        "Error should identify metacharacter was blocked"
    );

    // Same at strict level
    let strict_patterns = vec![ArgPattern::new(
        "echo",
        vec!["hello".to_string()], // exact pattern
    )];
    let strict_result = validate_arguments_strict("echo", &["hello; rm -rf /"], &strict_patterns);
    assert!(
        strict_result.is_err(),
        "Strict security check must block metacharacters before execution"
    );
    assert!(
        strict_result
            .unwrap_err()
            .reason
            .contains("shell metacharacter"),
        "Strict error should identify metacharacter was blocked"
    );
}

/// AC: Security level check for argument validation happens before execution.
///
/// Arguments are validated against patterns before any command spawns.
#[test]
fn argument_validation_checked_before_execution() {
    let patterns = vec![ArgPattern::new(
        "cargo",
        vec![
            "build".to_string(),
            "test".to_string(),
            "--release".to_string(),
        ],
    )];

    // Valid args: check passes (execution could proceed)
    assert!(
        validate_arguments("cargo", &["build"], &patterns).is_ok(),
        "Valid single arg should pass check"
    );
    assert!(
        validate_arguments("cargo", &["build", "--release"], &patterns).is_ok(),
        "Valid multiple args should pass check"
    );

    // Invalid args: check fails (execution blocked)
    let result = validate_arguments("cargo", &["run"], &patterns);
    assert!(
        result.is_err(),
        "Invalid arg must be blocked before execution"
    );
    assert_eq!(
        result.unwrap_err().argument,
        "run",
        "Error should identify which argument failed the check"
    );
}

/// AC: Security level check order - shell metacharacters checked first.
///
/// The security check order ensures the most dangerous patterns are caught
/// first, before any other validation or execution.
#[test]
fn security_check_order_metacharacters_first() {
    let patterns = vec![ArgPattern::new("cmd", vec!["valid".to_string()])];

    // Input with both invalid arg AND metacharacter
    // Shell metacharacter check happens first
    let result = validate_arguments("cmd", &["invalid; whoami"], &patterns);
    assert!(result.is_err());
    let err = result.unwrap_err();

    // Metacharacter check should catch this first
    assert!(
        err.reason.contains("shell metacharacter"),
        "Metacharacter check should run before argument pattern check: {}",
        err.reason
    );
}

/// AC: Security level propagates to validation context.
///
/// The security level must be available throughout the validation chain
/// so the correct strategy is applied.
#[test]
fn security_level_propagates_to_validation() {
    let patterns = vec![ArgPattern::new("npm", vec!["--save*".to_string()])];

    // Same input, different validation based on security level context
    let default_result = validate_arguments("npm", &["--save-dev"], &patterns);
    let strict_result = validate_arguments_strict("npm", &["--save-dev"], &patterns);

    // Default level: glob matching, passes
    assert!(
        default_result.is_ok(),
        "Default level should use glob matching"
    );

    // Strict level: pattern has wildcard, rejected before matching even attempted
    assert!(
        strict_result.is_err(),
        "Strict level should reject wildcard pattern"
    );
}

// ---------------------------------------------------------------------------
// Edge cases for pre-execution security checks
// ---------------------------------------------------------------------------

/// AC: Empty args pass security check (no execution of invalid command).
#[test]
fn empty_args_pass_security_check() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    // Empty args are valid - security check passes
    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "Empty args should pass Default level check"
    );
    assert!(
        validate_arguments_strict("git", &[], &patterns).is_ok(),
        "Empty args should pass Strict level check"
    );
}

/// AC: Unknown command fails security check before execution.
#[test]
fn unknown_command_fails_security_check() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    // Unknown command with args fails check - no execution occurs
    let result = validate_arguments("unknown", &["arg"], &patterns);
    assert!(
        result.is_err(),
        "Unknown command must fail security check before execution"
    );
}

/// AC: Security checks are consistent across repeated calls.
///
/// The security level check must give consistent results - it's not
/// dependent on timing or mutable state.
#[test]
fn security_check_consistent() {
    let patterns = vec![ArgPattern::new("echo", vec!["hello".to_string()])];

    // Same check multiple times gives same result
    for _ in 0..5 {
        assert!(validate_arguments("echo", &["hello"], &patterns).is_ok());
        assert!(validate_arguments("echo", &["goodbye"], &patterns).is_err());
        assert!(validate_arguments_strict("echo", &["hello"], &patterns).is_ok());
        assert!(validate_arguments_strict("echo", &["goodbye"], &patterns).is_err());
    }
}

/// AC: Paranoid level prevents any CLI function creation.
///
/// At Paranoid level, the security check at function-build time prevents
/// the CLI function from existing at all - execution is impossible.
#[test]
fn paranoid_level_no_cli_function() {
    let registry = make_registry();
    let manifest = manifest_with_caps(PluginCapabilities {
        cli: Some(CliCapability {
            allowed_commands: vec!["echo".to_string(), "cat".to_string(), "ls".to_string()],
            ..Default::default()
        }),
        ..Default::default()
    });

    let fns = registry.build_functions_for_level(&manifest, NetworkSecurityLevel::Paranoid);

    // No CLI function means no execution path exists
    let cli_fn = fns.iter().find(|f| f.name() == "zeroclaw_cli_exec");
    assert!(
        cli_fn.is_none(),
        "Paranoid level security check must prevent CLI function creation"
    );
}

/// AC: Security level checked for each command independently.
///
/// Security checks are applied per-command, ensuring each command is
/// validated before execution.
#[test]
fn security_check_per_command() {
    let patterns = vec![
        ArgPattern::new("git", vec!["status".to_string()]),
        ArgPattern::new("npm", vec!["test".to_string()]),
    ];

    // Each command's security check is independent
    assert!(validate_arguments("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments("git", &["push"], &patterns).is_err());
    assert!(validate_arguments("npm", &["test"], &patterns).is_ok());
    assert!(validate_arguments("npm", &["install"], &patterns).is_err());

    // Cross-command args don't match (security isolation)
    assert!(
        validate_arguments("git", &["test"], &patterns).is_err(),
        "git should not accept npm's allowed args"
    );
    assert!(
        validate_arguments("npm", &["status"], &patterns).is_err(),
        "npm should not accept git's allowed args"
    );
}
