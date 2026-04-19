#![cfg(feature = "plugins-wasm")]

//! Security test: arguments validated against allowed patterns.
//!
//! Task US-ZCL-54-4: Verifies acceptance criterion for US-ZCL-54:
//! > Arguments validated against allowed patterns
//!
//! These tests exercise the `validate_arguments` function from the security module
//! to ensure CLI arguments are validated against ArgPattern rules. Arguments must
//! match at least one pattern for the command being executed.

use zeroclaw::plugins::ArgPattern;
use zeroclaw::security::{ArgumentValidationError, validate_arguments};

// ---------------------------------------------------------------------------
// Core acceptance criterion: arguments must match allowed patterns
// ---------------------------------------------------------------------------

/// AC: Arguments matching exact patterns are accepted.
#[test]
fn exact_pattern_match_accepted() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string(), "diff".to_string()],
    )];

    assert!(
        validate_arguments("git", &["status"], &patterns).is_ok(),
        "exact pattern 'status' should be accepted"
    );
    assert!(
        validate_arguments("git", &["log"], &patterns).is_ok(),
        "exact pattern 'log' should be accepted"
    );
    assert!(
        validate_arguments("git", &["diff"], &patterns).is_ok(),
        "exact pattern 'diff' should be accepted"
    );
}

/// AC: Multiple arguments all matching patterns are accepted.
#[test]
fn multiple_matching_args_accepted() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec![
            "log".to_string(),
            "--oneline".to_string(),
            "-n".to_string(),
            "*".to_string(),
        ],
    )];

    assert!(
        validate_arguments("git", &["log", "--oneline"], &patterns).is_ok(),
        "multiple matching arguments should be accepted"
    );
    assert!(
        validate_arguments("git", &["log", "-n", "10"], &patterns).is_ok(),
        "multiple args including glob match should be accepted"
    );
}

/// AC: Arguments NOT matching any pattern are rejected.
#[test]
fn non_matching_arg_rejected() {
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
    assert!(
        err.reason.contains("does not match"),
        "error should say 'does not match': {}",
        err.reason
    );
    assert_eq!(
        err.argument, "push",
        "error should identify the failing argument"
    );
}

/// AC: If one argument in a list doesn't match, the entire call is rejected.
#[test]
fn partial_match_rejected() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string()],
    )];

    // First arg matches, second doesn't
    let result = validate_arguments("git", &["status", "push"], &patterns);
    assert!(result.is_err(), "partial match must be rejected");

    let err = result.unwrap_err();
    assert_eq!(
        err.argument, "push",
        "error should identify the non-matching argument"
    );
}

// ---------------------------------------------------------------------------
// Glob pattern support
// ---------------------------------------------------------------------------

/// AC: Wildcard (*) patterns match any argument.
#[test]
fn glob_wildcard_star_matches() {
    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "wildcard should match 'install'"
    );
    assert!(
        validate_arguments("npm", &["--save-dev"], &patterns).is_ok(),
        "wildcard should match '--save-dev'"
    );
    assert!(
        validate_arguments("npm", &["@org/package"], &patterns).is_ok(),
        "wildcard should match scoped packages"
    );
}

/// AC: Glob patterns with prefix/suffix work.
#[test]
fn glob_prefix_suffix_patterns() {
    let patterns = vec![ArgPattern::new(
        "ls",
        vec!["*.txt".to_string(), "*.rs".to_string(), "src/*".to_string()],
    )];

    assert!(
        validate_arguments("ls", &["file.txt"], &patterns).is_ok(),
        "'*.txt' should match 'file.txt'"
    );
    assert!(
        validate_arguments("ls", &["main.rs"], &patterns).is_ok(),
        "'*.rs' should match 'main.rs'"
    );
    assert!(
        validate_arguments("ls", &["src/lib.rs"], &patterns).is_ok(),
        "'src/*' should match 'src/lib.rs'"
    );
    assert!(
        validate_arguments("ls", &["file.md"], &patterns).is_err(),
        "'file.md' should not match any pattern"
    );
}

/// AC: Single character wildcard (?) matches exactly one character.
#[test]
fn glob_single_char_wildcard() {
    let patterns = vec![ArgPattern::new(
        "test",
        vec!["test?.txt".to_string(), "-?".to_string()],
    )];

    assert!(
        validate_arguments("test", &["test1.txt"], &patterns).is_ok(),
        "'test?.txt' should match 'test1.txt'"
    );
    assert!(
        validate_arguments("test", &["testA.txt"], &patterns).is_ok(),
        "'test?.txt' should match 'testA.txt'"
    );
    assert!(
        validate_arguments("test", &["-v"], &patterns).is_ok(),
        "'-?' should match '-v'"
    );
    assert!(
        validate_arguments("test", &["test12.txt"], &patterns).is_err(),
        "'test?.txt' should not match 'test12.txt' (two chars)"
    );
}

/// AC: Character class patterns [abc] work.
#[test]
fn glob_character_class() {
    let patterns = vec![ArgPattern::new(
        "cmd",
        vec!["file[123].txt".to_string(), "-[vVh]".to_string()],
    )];

    assert!(
        validate_arguments("cmd", &["file1.txt"], &patterns).is_ok(),
        "'file[123].txt' should match 'file1.txt'"
    );
    assert!(
        validate_arguments("cmd", &["file2.txt"], &patterns).is_ok(),
        "'file[123].txt' should match 'file2.txt'"
    );
    assert!(
        validate_arguments("cmd", &["-v"], &patterns).is_ok(),
        "'-[vVh]' should match '-v'"
    );
    assert!(
        validate_arguments("cmd", &["-V"], &patterns).is_ok(),
        "'-[vVh]' should match '-V'"
    );
    assert!(
        validate_arguments("cmd", &["file4.txt"], &patterns).is_err(),
        "'file[123].txt' should not match 'file4.txt'"
    );
}

// ---------------------------------------------------------------------------
// Commands without patterns
// ---------------------------------------------------------------------------

/// AC: Empty arguments accepted when no pattern defined for command.
#[test]
fn no_pattern_empty_args_accepted() {
    let patterns: Vec<ArgPattern> = vec![];

    assert!(
        validate_arguments("ls", &[], &patterns).is_ok(),
        "empty args should be accepted when no pattern defined"
    );
}

/// AC: Any argument rejected when no pattern defined for command.
#[test]
fn no_pattern_any_arg_rejected() {
    let patterns: Vec<ArgPattern> = vec![];

    let result = validate_arguments("ls", &["-la"], &patterns);
    assert!(
        result.is_err(),
        "arguments must be rejected when no pattern defined"
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("no argument patterns defined"),
        "error should mention no patterns defined: {}",
        err.reason
    );
}

/// AC: Pattern for different command doesn't apply.
#[test]
fn pattern_for_other_command_not_applied() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "*".to_string()],
    )];

    // "npm" has no pattern defined (only "git" does)
    let result = validate_arguments("npm", &["install"], &patterns);
    assert!(
        result.is_err(),
        "argument should be rejected when no pattern for this command"
    );

    let err = result.unwrap_err();
    assert!(
        err.reason
            .contains("no argument patterns defined for command 'npm'"),
        "error should specify the command: {}",
        err.reason
    );
}

// ---------------------------------------------------------------------------
// Empty arguments handling
// ---------------------------------------------------------------------------

/// AC: Empty arguments accepted when pattern exists (no args to validate).
#[test]
fn pattern_exists_empty_args_accepted() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "empty args should be accepted even when patterns exist"
    );
}

// ---------------------------------------------------------------------------
// Error type and messages
// ---------------------------------------------------------------------------

/// Error type is ArgumentValidationError with descriptive fields.
#[test]
fn error_type_and_fields() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    let result: Result<(), ArgumentValidationError> =
        validate_arguments("git", &["forbidden"], &patterns);

    let err = result.expect_err("should return ArgumentValidationError");

    assert_eq!(
        err.argument, "forbidden",
        "error should include the failing argument"
    );
    assert!(!err.reason.is_empty(), "error should have a reason");
}

/// Display implementation provides human-readable error.
#[test]
fn error_display_readable() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    let result = validate_arguments("git", &["blocked"], &patterns);
    let err = result.unwrap_err();
    let display = format!("{}", err);

    assert!(
        display.contains("blocked"),
        "display should include argument: {}",
        display
    );
    assert!(
        display.contains("validation failed"),
        "display should indicate validation failure: {}",
        display
    );
}

// ---------------------------------------------------------------------------
// Multiple patterns for same command
// ---------------------------------------------------------------------------

/// AC: Argument can match any one of multiple patterns.
#[test]
fn multiple_patterns_any_match() {
    let patterns = vec![ArgPattern::new(
        "cargo",
        vec![
            "build".to_string(),
            "test".to_string(),
            "--release".to_string(),
            "--features".to_string(),
            "*".to_string(),
        ],
    )];

    assert!(
        validate_arguments("cargo", &["build"], &patterns).is_ok(),
        "'build' should match"
    );
    assert!(
        validate_arguments("cargo", &["test"], &patterns).is_ok(),
        "'test' should match"
    );
    assert!(
        validate_arguments("cargo", &["build", "--release"], &patterns).is_ok(),
        "'build --release' should match"
    );
    assert!(
        validate_arguments("cargo", &["test", "--features", "wasm"], &patterns).is_ok(),
        "'test --features wasm' should match via wildcard"
    );
}

// ---------------------------------------------------------------------------
// Multiple ArgPatterns for different commands
// ---------------------------------------------------------------------------

/// AC: Correct pattern is selected based on command name.
#[test]
fn correct_pattern_selected_for_command() {
    let patterns = vec![
        ArgPattern::new("git", vec!["status".to_string(), "log".to_string()]),
        ArgPattern::new("npm", vec!["install".to_string(), "test".to_string()]),
        ArgPattern::new("cargo", vec!["build".to_string(), "run".to_string()]),
    ];

    // git: status allowed, install not
    assert!(validate_arguments("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments("git", &["install"], &patterns).is_err());

    // npm: install allowed, status not
    assert!(validate_arguments("npm", &["install"], &patterns).is_ok());
    assert!(validate_arguments("npm", &["status"], &patterns).is_err());

    // cargo: build allowed, status not
    assert!(validate_arguments("cargo", &["build"], &patterns).is_ok());
    assert!(validate_arguments("cargo", &["status"], &patterns).is_err());
}

// ---------------------------------------------------------------------------
// Edge cases
// ---------------------------------------------------------------------------

/// Empty pattern list in ArgPattern means no arguments allowed.
#[test]
fn empty_patterns_in_argpattern() {
    let patterns = vec![ArgPattern::new("git", vec![])];

    // Empty args should still pass (no args to validate)
    assert!(
        validate_arguments("git", &[], &patterns).is_ok(),
        "empty args with empty patterns should be ok"
    );

    // But any argument should fail
    let result = validate_arguments("git", &["status"], &patterns);
    assert!(
        result.is_err(),
        "any arg should be rejected with empty pattern list"
    );
}

/// Arguments with special but safe characters are handled.
#[test]
fn safe_special_characters_in_args() {
    let patterns = vec![ArgPattern::new("git", vec!["*".to_string()])];

    // These don't contain shell metacharacters
    assert!(validate_arguments("git", &["HEAD~1"], &patterns).is_ok());
    assert!(validate_arguments("git", &["feature/add-login"], &patterns).is_ok());
    assert!(validate_arguments("git", &["--author=Name"], &patterns).is_ok());
    assert!(validate_arguments("git", &["file with spaces.txt"], &patterns).is_ok());
}

/// Case sensitivity in pattern matching.
#[test]
fn case_sensitive_matching() {
    let patterns = vec![ArgPattern::new("cmd", vec!["Status".to_string()])];

    assert!(
        validate_arguments("cmd", &["Status"], &patterns).is_ok(),
        "exact case should match"
    );
    assert!(
        validate_arguments("cmd", &["status"], &patterns).is_err(),
        "different case should not match"
    );
    assert!(
        validate_arguments("cmd", &["STATUS"], &patterns).is_err(),
        "uppercase should not match"
    );
}
