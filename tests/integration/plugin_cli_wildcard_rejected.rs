#![cfg(feature = "plugins-wasm")]

//! Security test: wildcards rejected in command allowlists at all security levels.
//!
//! Task US-ZCL-54-2: Verifies acceptance criterion for US-ZCL-54:
//! > Wildcards rejected at all security levels
//!
//! These tests exercise the `validate_command_allowlist` function from the security
//! module to ensure wildcards (`*`) are unconditionally rejected in plugin command
//! allowlists. Unlike tool delegation wildcards which may be allowed at relaxed/default
//! levels, command allowlist wildcards are ALWAYS rejected as they would allow plugins
//! to execute arbitrary system commands.

use zeroclaw::security::{CommandNotAllowedError, validate_command_allowlist};

// ---------------------------------------------------------------------------
// Core acceptance criterion: wildcards rejected at ALL security levels
// ---------------------------------------------------------------------------

/// AC: Bare wildcard (*) is rejected unconditionally.
/// This is the primary security invariant - plugins cannot use "*" to run any command.
#[test]
fn bare_wildcard_rejected() {
    let allowed = vec!["*".to_string()];
    let result = validate_command_allowlist("ls", &allowed);

    assert!(
        result.is_err(),
        "wildcard must be rejected, got success with path: {:?}",
        result.ok()
    );

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("wildcard"),
        "error should mention wildcards: {}",
        err.reason
    );
}

/// AC: Wildcard with leading/trailing whitespace is rejected.
/// Prevents bypassing the check via `" * "` or similar.
#[test]
fn wildcard_with_whitespace_rejected() {
    let test_cases = [
        " *".to_string(),
        "* ".to_string(),
        " * ".to_string(),
        "\t*".to_string(),
        "*\t".to_string(),
        "\t*\t".to_string(),
    ];

    for allowed in test_cases {
        let debug = allowed.escape_debug().to_string();
        let result = validate_command_allowlist("ls", &[allowed]);
        assert!(
            result.is_err(),
            "wildcard with whitespace '{}' must be rejected",
            debug
        );
    }
}

/// AC: Wildcard mixed with legitimate commands is still rejected.
/// The presence of ANY wildcard in the allowlist invalidates the entire list.
#[test]
fn wildcard_mixed_with_valid_commands_rejected() {
    let allowed = vec![
        "ls".to_string(),
        "cat".to_string(),
        "*".to_string(), // Wildcard hidden among valid commands
        "echo".to_string(),
    ];
    let result = validate_command_allowlist("ls", &allowed);

    assert!(
        result.is_err(),
        "wildcard mixed with valid commands must be rejected"
    );
}

/// AC: Wildcard at different positions in list is rejected.
#[test]
fn wildcard_at_any_position_rejected() {
    // At start
    let result = validate_command_allowlist("ls", &["*".into(), "cat".into()]);
    assert!(result.is_err(), "wildcard at start must be rejected");

    // At end
    let result = validate_command_allowlist("ls", &["cat".into(), "*".into()]);
    assert!(result.is_err(), "wildcard at end must be rejected");

    // In middle
    let result = validate_command_allowlist("ls", &["cat".into(), "*".into(), "echo".into()]);
    assert!(result.is_err(), "wildcard in middle must be rejected");
}

// ---------------------------------------------------------------------------
// Contrast: legitimate allowlists work correctly
// ---------------------------------------------------------------------------

/// Sanity check: explicit command lists work when no wildcards present.
#[test]
fn explicit_commands_allowed() {
    let allowed = vec!["ls".to_string(), "cat".to_string()];
    let result = validate_command_allowlist("ls", &allowed);

    assert!(
        result.is_ok(),
        "explicit command list should work: {:?}",
        result.err()
    );

    let path = result.unwrap();
    assert!(
        path.is_absolute(),
        "resolved path should be absolute: {:?}",
        path
    );
}

/// Sanity check: commands not in allowlist are rejected (not wildcard-related).
#[test]
fn unlisted_command_rejected_with_different_error() {
    let allowed = vec!["ls".to_string()];
    let result = validate_command_allowlist("cat", &allowed);

    assert!(result.is_err(), "unlisted command should be rejected");

    let err = result.unwrap_err();
    assert!(
        err.reason.contains("not in allowlist"),
        "error should say 'not in allowlist', not mention wildcards: {}",
        err.reason
    );
    // Notably, this error should NOT mention "wildcard"
    assert!(
        !err.reason.contains("wildcard"),
        "non-wildcard rejection should not mention wildcards: {}",
        err.reason
    );
}

// ---------------------------------------------------------------------------
// Security level independence (the "all levels" part of the AC)
// ---------------------------------------------------------------------------

/// AC: Wildcards rejected regardless of security level configuration.
/// This test verifies the behavior is unconditional and not affected by any
/// security level settings. The function itself doesn't take a security level
/// parameter because the behavior must be the same at ALL levels.
#[test]
fn wildcard_rejection_is_unconditional() {
    // The validate_command_allowlist function does not accept a security level
    // parameter - this is intentional. Wildcard rejection is hardcoded and
    // unconditional. This test verifies that calling the function multiple times
    // with wildcard allowlists always fails, demonstrating level independence.

    let wildcard_lists: Vec<Vec<String>> = vec![
        vec!["*".to_string()],
        vec!["*".to_string(), "ls".to_string()],
        vec!["ls".to_string(), "*".to_string()],
        vec![" * ".to_string()],
    ];

    let commands = ["ls", "cat", "echo", "sh"];

    for allowed in &wildcard_lists {
        for cmd in &commands {
            let result = validate_command_allowlist(cmd, allowed);
            assert!(
                result.is_err(),
                "wildcard must be rejected for command '{}' with allowlist {:?}",
                cmd,
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
}

// ---------------------------------------------------------------------------
// Error type validation
// ---------------------------------------------------------------------------

/// Error type is CommandNotAllowedError with descriptive message.
#[test]
fn error_type_and_message() {
    let result: Result<_, CommandNotAllowedError> =
        validate_command_allowlist("ls", &["*".to_string()]);

    let err = result.expect_err("should return CommandNotAllowedError");

    assert_eq!(err.command, "ls", "error should include the command name");

    // The reason should be descriptive and mention the security concern
    assert!(
        err.reason.contains("wildcard"),
        "reason should explain wildcards are not allowed"
    );
    assert!(
        err.reason.contains("plugin") || err.reason.contains("allowlist"),
        "reason should mention plugin context: {}",
        err.reason
    );
}

/// Display implementation provides human-readable error.
#[test]
fn error_display_readable() {
    let result = validate_command_allowlist("rm", &["*".to_string()]);
    let err = result.unwrap_err();
    let display = format!("{}", err);

    assert!(
        display.contains("rm"),
        "display should include command name: {}",
        display
    );
    assert!(
        display.contains("not allowed") || display.contains("wildcard"),
        "display should explain the rejection: {}",
        display
    );
}

// ---------------------------------------------------------------------------
// Edge cases and attack patterns
// ---------------------------------------------------------------------------

/// Glob-style patterns (not wildcards) should not trigger wildcard rejection.
/// Note: These may fail for other reasons (not in allowlist) but NOT for wildcards.
#[test]
fn glob_patterns_not_treated_as_bare_wildcard() {
    // These contain '*' but are not bare wildcards
    let patterns = ["*.txt", "file*", "*name*", "test-*-file", "/bin/*", "ls*"];

    for pattern in patterns {
        let result = validate_command_allowlist("ls", &[pattern.to_string()]);

        // These should fail for "not in allowlist", not "wildcards"
        if let Err(err) = result {
            // The error might be "command not found" for the pattern itself,
            // or "not in allowlist" - but NOT "wildcards not allowed"
            // because these aren't bare wildcards
            let is_wildcard_error = err.reason.contains("wildcard");

            // A bare "*" and " * " should trigger wildcard error,
            // but glob patterns like "*.txt" should not
            assert!(
                !is_wildcard_error,
                "glob pattern '{}' should not be treated as bare wildcard, error: {}",
                pattern, err.reason
            );
        }
    }
}

/// Single asterisk in various forms is always rejected.
#[test]
fn single_asterisk_variations_rejected() {
    let variations = [
        "*",     // bare
        " *",    // leading space
        "* ",    // trailing space
        " * ",   // both spaces
        "\t*",   // leading tab
        "*\t",   // trailing tab
        "\t*\t", // both tabs
        "\n*",   // leading newline
        "*\n",   // trailing newline
    ];

    for var in variations {
        let result = validate_command_allowlist("ls", &[var.to_string()]);
        assert!(
            result.is_err(),
            "asterisk variation '{}' must be rejected",
            var.escape_debug()
        );
    }
}
