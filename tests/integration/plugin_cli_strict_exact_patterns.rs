#![cfg(feature = "plugins-wasm")]

//! Test: Strict level requires exact command+arg patterns.
//!
//! Task US-ZCL-58-2: Verifies acceptance criterion for US-ZCL-58:
//! > Strict level requires exact command+arg patterns
//!
//! These tests verify that at Strict security level:
//! 1. Wildcard patterns (*, ?, []) in the manifest are rejected
//! 2. Arguments must exactly match patterns (no glob expansion)
//! 3. Partial matches are rejected

use zeroclaw::plugins::ArgPattern;
use zeroclaw::security::{validate_arguments, validate_arguments_strict};

// ---------------------------------------------------------------------------
// Core acceptance criterion: Strict level requires exact patterns
// ---------------------------------------------------------------------------

/// AC: Strict level accepts exact pattern matches.
#[test]
fn strict_level_accepts_exact_match() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string(), "diff".to_string()],
    )];

    assert!(
        validate_arguments_strict("git", &["status"], &patterns).is_ok(),
        "exact pattern 'status' should be accepted at Strict level"
    );
    assert!(
        validate_arguments_strict("git", &["log"], &patterns).is_ok(),
        "exact pattern 'log' should be accepted at Strict level"
    );
    assert!(
        validate_arguments_strict("git", &["diff"], &patterns).is_ok(),
        "exact pattern 'diff' should be accepted at Strict level"
    );
}

/// AC: Strict level rejects wildcard star (*) patterns.
#[test]
fn strict_level_rejects_wildcard_star_pattern() {
    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    let result = validate_arguments_strict("npm", &["install"], &patterns);
    assert!(
        result.is_err(),
        "Strict level must reject wildcard * pattern"
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("wildcard"),
        "error should mention wildcard rejection: {}",
        err.reason
    );
    assert!(
        err.reason.contains("Strict"),
        "error should mention Strict security level: {}",
        err.reason
    );
}

/// AC: Strict level rejects glob prefix patterns (*.txt).
#[test]
fn strict_level_rejects_glob_prefix_pattern() {
    let patterns = vec![ArgPattern::new(
        "ls",
        vec!["*.txt".to_string(), "*.rs".to_string()],
    )];

    let result = validate_arguments_strict("ls", &["file.txt"], &patterns);
    assert!(
        result.is_err(),
        "Strict level must reject glob prefix pattern *.txt"
    );
    assert!(result.unwrap_err().reason.contains("wildcard"));
}

/// AC: Strict level rejects single-character wildcard (?) patterns.
#[test]
fn strict_level_rejects_single_char_wildcard() {
    let patterns = vec![ArgPattern::new("cmd", vec!["-?".to_string()])];

    let result = validate_arguments_strict("cmd", &["-v"], &patterns);
    assert!(
        result.is_err(),
        "Strict level must reject single-char wildcard pattern -?"
    );
    assert!(result.unwrap_err().reason.contains("wildcard"));
}

/// AC: Strict level rejects character class patterns ([abc]).
#[test]
fn strict_level_rejects_character_class_pattern() {
    let patterns = vec![ArgPattern::new("cmd", vec!["file[123].txt".to_string()])];

    let result = validate_arguments_strict("cmd", &["file1.txt"], &patterns);
    assert!(
        result.is_err(),
        "Strict level must reject character class pattern [123]"
    );
    assert!(result.unwrap_err().reason.contains("wildcard"));
}

/// AC: Strict level requires exact string equality, not glob matching.
#[test]
fn strict_level_requires_exact_equality_not_glob() {
    // Even if a value could be interpreted as matching via glob,
    // Strict level requires literal equality
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["--verbose".to_string(), "-v".to_string()],
    )];

    // Exact matches pass
    assert!(
        validate_arguments_strict("git", &["--verbose"], &patterns).is_ok(),
        "--verbose should match exactly"
    );
    assert!(
        validate_arguments_strict("git", &["-v"], &patterns).is_ok(),
        "-v should match exactly"
    );

    // Non-exact fails (--verbose=true doesn't equal --verbose)
    let result = validate_arguments_strict("git", &["--verbose=true"], &patterns);
    assert!(
        result.is_err(),
        "--verbose=true should not match --verbose at Strict level"
    );
}

/// AC: Strict level rejects arguments not in the exact pattern list.
#[test]
fn strict_level_rejects_non_matching_args() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string()],
    )];

    let result = validate_arguments_strict("git", &["push"], &patterns);
    assert!(
        result.is_err(),
        "'push' must be rejected when not in exact patterns"
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("Strict mode"),
        "error should mention Strict mode: {}",
        err.reason
    );
    assert_eq!(
        err.argument, "push",
        "error should identify the failing argument"
    );
}

/// AC: Strict level rejects partial matches (one valid, one invalid arg).
#[test]
fn strict_level_rejects_partial_match() {
    let patterns = vec![ArgPattern::new(
        "git",
        vec!["status".to_string(), "log".to_string()],
    )];

    // First arg matches, second doesn't
    let result = validate_arguments_strict("git", &["status", "push"], &patterns);
    assert!(
        result.is_err(),
        "partial match must be rejected at Strict level"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.argument, "push",
        "error should identify the non-matching argument"
    );
}

// ---------------------------------------------------------------------------
// Contrast with Default level (glob patterns allowed)
// ---------------------------------------------------------------------------

/// Contrast: Default level accepts wildcard patterns (Strict rejects them).
#[test]
fn default_level_accepts_wildcard_patterns() {
    let patterns = vec![ArgPattern::new("npm", vec!["*".to_string()])];

    // Default level uses glob matching
    assert!(
        validate_arguments("npm", &["install"], &patterns).is_ok(),
        "Default level should accept wildcard * pattern"
    );
    assert!(
        validate_arguments("npm", &["--save-dev"], &patterns).is_ok(),
        "Default level wildcard should match any argument"
    );
}

/// Contrast: Default level accepts glob prefix patterns (Strict rejects them).
#[test]
fn default_level_accepts_glob_prefix_patterns() {
    let patterns = vec![ArgPattern::new(
        "ls",
        vec!["*.txt".to_string(), "*.rs".to_string()],
    )];

    // Default level matches via glob
    assert!(
        validate_arguments("ls", &["file.txt"], &patterns).is_ok(),
        "Default level should match *.txt"
    );
    assert!(
        validate_arguments("ls", &["main.rs"], &patterns).is_ok(),
        "Default level should match *.rs"
    );
}

// ---------------------------------------------------------------------------
// Edge cases at Strict level
// ---------------------------------------------------------------------------

/// AC: Empty arguments accepted at Strict level when patterns exist.
#[test]
fn strict_level_accepts_empty_args() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    assert!(
        validate_arguments_strict("git", &[], &patterns).is_ok(),
        "empty args should be accepted at Strict level"
    );
}

/// AC: Strict level rejects args when no pattern defined for command.
#[test]
fn strict_level_rejects_args_without_pattern() {
    let patterns: Vec<ArgPattern> = vec![];

    let result = validate_arguments_strict("ls", &["-la"], &patterns);
    assert!(
        result.is_err(),
        "arguments must be rejected when no pattern defined"
    );
}

/// AC: Multiple exact patterns all work at Strict level.
#[test]
fn strict_level_multiple_exact_patterns() {
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

    assert!(
        validate_arguments_strict("cargo", &["build"], &patterns).is_ok(),
        "'build' should match"
    );
    assert!(
        validate_arguments_strict("cargo", &["test"], &patterns).is_ok(),
        "'test' should match"
    );
    assert!(
        validate_arguments_strict("cargo", &["build", "--release"], &patterns).is_ok(),
        "'build --release' should match"
    );
    assert!(
        validate_arguments_strict("cargo", &["test", "--features", "wasm"], &patterns).is_ok(),
        "'test --features wasm' should match"
    );
}

/// AC: Case sensitivity is preserved at Strict level.
#[test]
fn strict_level_case_sensitive() {
    let patterns = vec![ArgPattern::new("cmd", vec!["Status".to_string()])];

    assert!(
        validate_arguments_strict("cmd", &["Status"], &patterns).is_ok(),
        "exact case should match"
    );
    assert!(
        validate_arguments_strict("cmd", &["status"], &patterns).is_err(),
        "lowercase should not match"
    );
    assert!(
        validate_arguments_strict("cmd", &["STATUS"], &patterns).is_err(),
        "uppercase should not match"
    );
}

/// AC: Strict level still rejects shell metacharacters first.
#[test]
fn strict_level_rejects_shell_metacharacters() {
    let patterns = vec![ArgPattern::new("git", vec!["status".to_string()])];

    let result = validate_arguments_strict("git", &["status; rm -rf /"], &patterns);
    assert!(result.is_err(), "shell metacharacters must be rejected");
    assert!(
        result.unwrap_err().reason.contains("shell metacharacter"),
        "error should mention shell metacharacter"
    );
}

/// AC: Patterns for different commands are correctly isolated.
#[test]
fn strict_level_command_pattern_isolation() {
    let patterns = vec![
        ArgPattern::new("git", vec!["status".to_string(), "log".to_string()]),
        ArgPattern::new("npm", vec!["install".to_string(), "test".to_string()]),
    ];

    // git patterns work for git
    assert!(validate_arguments_strict("git", &["status"], &patterns).is_ok());
    assert!(validate_arguments_strict("git", &["install"], &patterns).is_err());

    // npm patterns work for npm
    assert!(validate_arguments_strict("npm", &["install"], &patterns).is_ok());
    assert!(validate_arguments_strict("npm", &["status"], &patterns).is_err());
}
