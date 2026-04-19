#![cfg(feature = "plugins-wasm")]

//! Test: Allowlist enforcement for CLI execution.
//!
//! Task US-ZCL-60-11: Verifies allowlist enforcement:
//! - Unauthorized commands are rejected
//! - Allowed commands work
//! - Exact match vs pattern match behavior

use zeroclaw::plugins::ArgPattern;
use zeroclaw::security::{
    validate_arguments, validate_arguments_strict, validate_command_allowlist,
};

// ---------------------------------------------------------------------------
// Core: Unauthorized commands are rejected
// ---------------------------------------------------------------------------

/// AC: Command not in allowlist is rejected.
#[test]
fn unauthorized_command_rejected() {
    let allowed = vec!["ls".to_string(), "echo".to_string()];

    let result = validate_command_allowlist("rm", &allowed);
    assert!(
        result.is_err(),
        "'rm' must be rejected when not in allowlist"
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("not in allowlist"),
        "error should mention 'not in allowlist': {}",
        err.reason
    );
    assert_eq!(
        err.command, "rm",
        "error should identify the rejected command"
    );
}

/// AC: Multiple unauthorized commands are all rejected.
#[test]
fn multiple_unauthorized_commands_rejected() {
    let allowed = vec!["echo".to_string()];
    let unauthorized = ["rm", "cat", "curl", "wget", "bash", "sh"];

    for cmd in unauthorized {
        let result = validate_command_allowlist(cmd, &allowed);
        assert!(
            result.is_err(),
            "'{}' must be rejected when not in allowlist",
            cmd
        );
    }
}

/// AC: Empty allowlist rejects all commands.
#[test]
fn empty_allowlist_rejects_all() {
    let allowed: Vec<String> = vec![];

    let result = validate_command_allowlist("ls", &allowed);
    assert!(
        result.is_err(),
        "empty allowlist should reject all commands"
    );
}

// ---------------------------------------------------------------------------
// Core: Allowed commands work
// ---------------------------------------------------------------------------

/// AC: Command in allowlist is accepted.
#[test]
fn allowed_command_accepted() {
    let allowed = vec!["ls".to_string(), "echo".to_string(), "cat".to_string()];

    let result = validate_command_allowlist("echo", &allowed);
    assert!(
        result.is_ok(),
        "'echo' should be accepted when in allowlist: {:?}",
        result.err()
    );
}

/// AC: All commands in allowlist are accepted.
#[test]
fn all_allowed_commands_work() {
    let allowed = vec![
        "ls".to_string(),
        "echo".to_string(),
        "cat".to_string(),
        "head".to_string(),
        "tail".to_string(),
    ];

    for cmd in &allowed {
        let result = validate_command_allowlist(cmd, &allowed);
        assert!(
            result.is_ok(),
            "'{}' should be accepted when in allowlist: {:?}",
            cmd,
            result.err()
        );
    }
}

/// AC: Allowlist with single entry works.
#[test]
fn single_entry_allowlist_works() {
    let allowed = vec!["echo".to_string()];

    assert!(
        validate_command_allowlist("echo", &allowed).is_ok(),
        "'echo' should be accepted in single-entry allowlist"
    );
    assert!(
        validate_command_allowlist("ls", &allowed).is_err(),
        "'ls' should be rejected from single-entry allowlist"
    );
}

// ---------------------------------------------------------------------------
// Core: Exact match vs pattern match (arguments)
// ---------------------------------------------------------------------------

/// AC: Exact argument match works at all levels.
#[test]
fn exact_argument_match_works() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string(), "diff".to_string()],
    )];

    // Exact matches work at default level
    assert!(
        validate_arguments("git", &["status"], &patterns).is_ok(),
        "exact match 'status' should pass"
    );
    assert!(
        validate_arguments("git", &["log"], &patterns).is_ok(),
        "exact match 'log' should pass"
    );

    // Exact matches work at strict level
    assert!(
        validate_arguments_strict("git", &["status"], &patterns).is_ok(),
        "exact match 'status' should pass at strict level"
    );
}

/// AC: Non-matching argument is rejected.
#[test]
fn non_matching_argument_rejected() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string()],
    )];

    let result = validate_arguments("git", &["push"], &patterns);
    assert!(
        result.is_err(),
        "'push' must be rejected when not in patterns"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.argument, "push",
        "error should identify rejected argument"
    );
}

/// AC: Glob pattern matching works at default level.
#[test]
fn glob_pattern_matching_at_default_level() {
    let patterns = vec![ArgPattern::new(
        "npm",
        vec!["install".to_string(), "--save*".to_string()],
    )];

    // Exact match
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "exact 'install' should match"
    );

    // Glob pattern matches
    assert!(
        validate_arguments("npm", &["--save-dev"], &patterns).is_ok(),
        "'--save*' should match '--save-dev'"
    );
    assert!(
        validate_arguments("npm", &["--save-exact"], &patterns).is_ok(),
        "'--save*' should match '--save-exact'"
    );
    assert!(
        validate_arguments("npm", &["--save"], &patterns).is_ok(),
        "'--save*' should match '--save'"
    );

    // Non-matching rejected
    assert!(
        validate_arguments("npm", &["--production"], &patterns).is_err(),
        "'--production' should not match '--save*'"
    );
}

/// AC: Strict level requires exact matches (no glob expansion).
#[test]
fn strict_level_requires_exact_match() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "--verbose".to_string()],
    )];

    // Exact match works
    assert!(
        validate_arguments_strict("git", &["status"], &patterns).is_ok(),
        "exact 'status' should match at strict"
    );
    assert!(
        validate_arguments_strict("git", &["--verbose"], &patterns).is_ok(),
        "exact '--verbose' should match at strict"
    );

    // Partial/extended match fails at strict
    let result = validate_arguments_strict("git", &["--verbose=true"], &patterns);
    assert!(
        result.is_err(),
        "'--verbose=true' should not match '--verbose' at strict level"
    );
}

/// AC: Strict level rejects wildcard patterns entirely.
#[test]
fn strict_rejects_wildcard_patterns() {
    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    // Default level allows wildcard
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "wildcard should work at default level"
    );

    // Strict level rejects wildcard
    let result = validate_arguments_strict("npm", &["install"], &patterns);
    assert!(result.is_err(), "wildcard must be rejected at strict level");
    assert!(
        result.unwrap_err().reason.contains("wildcard"),
        "error should mention wildcard"
    );
}

/// AC: Multiple arguments all validated.
#[test]
fn multiple_arguments_all_validated() {
    let patterns = vec![ArgPattern::new(
        "cargo",
        vec![
            "build".to_string(),
            "test".to_string(),
            "--release".to_string(),
            "--features".to_string(),
            "wasm".to_string(),
        ],
    )];

    // Multiple valid args pass
    assert!(
        validate_arguments("cargo", &["build", "--release"], &patterns).is_ok(),
        "multiple valid args should pass"
    );
    assert!(
        validate_arguments("cargo", &["test", "--features", "wasm"], &patterns).is_ok(),
        "multiple valid args should pass"
    );

    // One invalid arg fails entire check
    let result = validate_arguments("cargo", &["build", "--invalid"], &patterns);
    assert!(
        result.is_err(),
        "one invalid arg should fail entire validation"
    );
    assert_eq!(
        result.unwrap_err().argument,
        "--invalid",
        "error should identify invalid argument"
    );
}

/// AC: Empty arguments always pass validation.
#[test]
fn empty_arguments_pass() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "empty args should pass at default level"
    );
    assert!(
        validate_arguments_strict("git", &[], &patterns).is_ok(),
        "empty args should pass at strict level"
    );
}

// ---------------------------------------------------------------------------
// Command allowlist vs argument patterns distinction
// ---------------------------------------------------------------------------

/// AC: Command allowlist wildcard is always rejected (security invariant).
#[test]
fn command_wildcard_always_rejected() {
    // Bare wildcard in command allowlist
    let result = validate_command_allowlist("ls", &["*".to_string()]);
    assert!(
        result.is_err(),
        "wildcard in command allowlist must be rejected"
    );
    assert!(
        result.unwrap_err().reason.contains("wildcard"),
        "error should mention wildcards"
    );

    // Wildcard mixed with valid commands
    let result = validate_command_allowlist("ls", &["echo".to_string(), "*".to_string()]);
    assert!(
        result.is_err(),
        "wildcard mixed with valid commands must be rejected"
    );

    // Whitespace-padded wildcard
    let result = validate_command_allowlist("ls", &[" * ".to_string()]);
    assert!(
        result.is_err(),
        "whitespace-padded wildcard must be rejected"
    );
}

/// AC: Argument pattern wildcards are distinct from command wildcards.
#[test]
fn argument_wildcards_allowed_command_wildcards_rejected() {
    // Command allowlist: wildcards ALWAYS rejected
    let cmd_result = validate_command_allowlist("ls", &["*".to_string()]);
    assert!(cmd_result.is_err(), "command wildcard must be rejected");

    // Argument patterns: wildcards allowed at default level
    let arg_patterns = vec![ArgPattern::new("git", vec!["*".to_string()])];
    let arg_result = validate_arguments("git", &["status"], &arg_patterns);
    assert!(
        arg_result.is_ok(),
        "argument wildcard is allowed at default: {:?}",
        arg_result.err()
    );
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// AC: Case sensitivity is preserved.
#[test]
fn case_sensitivity_preserved() {
    let patterns = vec![ArgPattern::new("cmd", vec!["Status".to_string()])];

    assert!(
        validate_arguments("cmd", &["Status"], &patterns).is_ok(),
        "exact case should match"
    );
    assert!(
        validate_arguments("cmd", &["status"], &patterns).is_err(),
        "lowercase should not match"
    );
    assert!(
        validate_arguments("cmd", &["STATUS"], &patterns).is_err(),
        "uppercase should not match"
    );
}

/// AC: Command patterns are isolated per command.
#[test]
fn command_patterns_isolated() {
    let patterns = vec![
        ArgPattern::new("git", vec!["status".to_string(), "log".to_string()]),
        ArgPattern::new("npm", vec!["install".to_string(), "test".to_string()]),
    ];

    // git patterns only match git
    assert!(validate_arguments("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments("git", &["install"], &patterns).is_err());

    // npm patterns only match npm
    assert!(validate_arguments("npm", &["install"], &patterns).is_ok());
    assert!(validate_arguments("npm", &["status"], &patterns).is_err());
}

/// AC: Shell metacharacters are always blocked (security invariant).
#[test]
fn shell_metacharacters_always_blocked() {
    // Even with broad pattern, metacharacters blocked
    let patterns = vec![ArgPattern::new("echo", vec!["*".to_string()])];

    let dangerous = [
        "hello; rm -rf /",
        "$(cat /etc/passwd)",
        "`id`",
        "test && evil",
        "a | b",
        "x > /dev/null",
    ];

    for arg in dangerous {
        let result = validate_arguments("echo", &[arg], &patterns);
        assert!(
            result.is_err(),
            "shell metacharacter '{}' must be rejected",
            arg.chars().take(20).collect::<String>()
        );
        assert!(
            result.unwrap_err().reason.contains("shell metacharacter"),
            "error should mention shell metacharacter"
        );
    }
}
