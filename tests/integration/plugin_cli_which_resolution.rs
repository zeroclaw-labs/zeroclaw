#![cfg(feature = "plugins-wasm")]

//! Security test: command path resolution via `which`.
//!
//! Task US-ZCL-54-1: Verifies acceptance criterion for US-ZCL-54:
//! > Commands resolved via which to get absolute paths
//!
//! These tests exercise the `resolve_command_path` function from the security
//! module to ensure commands are resolved to absolute paths using the system's
//! `which` utility. This prevents plugins from executing aliased or relative
//! commands that could bypass allowlist enforcement.

use std::path::PathBuf;
use zeroclaw::security::resolve_command_path;

// ---------------------------------------------------------------------------
// Core acceptance criterion: commands resolved to absolute paths
// ---------------------------------------------------------------------------

/// AC: Resolved paths are absolute, not relative.
/// This ensures plugins cannot use aliased or relative command paths.
#[test]
fn resolved_paths_are_absolute() {
    // `ls` is universally available on Unix systems
    let path = resolve_command_path("ls").expect("ls should be resolvable");
    assert!(
        path.is_absolute(),
        "resolved path must be absolute, got: {:?}",
        path
    );
}

/// AC: Common system commands resolve successfully.
/// These commands should exist on all Unix-like systems.
#[test]
fn common_commands_resolve() {
    let commands = ["ls", "sh", "cat", "echo"];

    for cmd in commands {
        let result = resolve_command_path(cmd);
        assert!(
            result.is_ok(),
            "common command '{}' should resolve, got error: {:?}",
            cmd,
            result.err()
        );

        let path = result.unwrap();
        assert!(
            path.is_absolute(),
            "{} path must be absolute, got: {:?}",
            cmd,
            path
        );
    }
}

/// AC: Resolved path contains the command name.
/// Sanity check that the path actually points to the right command.
#[test]
fn resolved_path_contains_command_name() {
    let path = resolve_command_path("ls").expect("ls should be resolvable");
    let path_str = path.to_string_lossy();

    assert!(
        path_str.ends_with("/ls") || path_str.ends_with("\\ls"),
        "resolved path should end with command name, got: {:?}",
        path
    );
}

/// AC: Resolution uses system `which` utility.
/// The resolved path should match what `which` returns.
#[test]
fn resolution_matches_system_which() {
    use std::process::Command;

    // Get expected path from `which ls` directly
    let which_output = Command::new("which")
        .arg("ls")
        .output()
        .expect("which command should execute");

    if !which_output.status.success() {
        panic!("system 'which ls' failed - cannot verify resolution");
    }

    let expected_path = String::from_utf8_lossy(&which_output.stdout)
        .trim()
        .to_string();
    let expected = PathBuf::from(&expected_path);

    // Verify our function returns the same result
    let resolved = resolve_command_path("ls").expect("ls should be resolvable");

    assert_eq!(
        resolved, expected,
        "resolve_command_path should return same path as system 'which'"
    );
}

// ---------------------------------------------------------------------------
// Non-existent command handling
// ---------------------------------------------------------------------------

/// AC: Non-existent commands return appropriate errors.
/// The function must not succeed for commands that don't exist.
#[test]
fn nonexistent_command_returns_error() {
    let result = resolve_command_path("nonexistent_command_xyz_12345_abc");
    assert!(
        result.is_err(),
        "non-existent command should return an error"
    );

    let err = result.unwrap_err();
    assert_eq!(
        err.command, "nonexistent_command_xyz_12345_abc",
        "error should contain the command name"
    );
}

/// AC: Error type is CommandNotFoundError with proper display.
#[test]
fn error_type_and_display() {
    let result = resolve_command_path("fake_command_that_does_not_exist");

    match result {
        Err(ref err) => {
            assert_eq!(err.command, "fake_command_that_does_not_exist");
            let err_msg = format!("{}", err);
            assert!(
                err_msg.contains("command not found"),
                "error message should indicate command not found: {}",
                err_msg
            );
        }
        Ok(_) => panic!("expected CommandNotFoundError, got success"),
    }
}

// ---------------------------------------------------------------------------
// Security invariants
// ---------------------------------------------------------------------------

/// Security: Relative commands are not accepted as valid results.
/// Even if somehow a relative path were returned, it must be rejected.
#[test]
fn relative_paths_rejected() {
    // Test that the function only returns absolute paths
    // by verifying with known commands
    for cmd in ["ls", "sh", "cat"] {
        if let Ok(path) = resolve_command_path(cmd) {
            assert!(
                !path.is_relative(),
                "resolve_command_path must never return relative paths, got: {:?}",
                path
            );
        }
    }
}

/// Security: Commands with path separators are resolved correctly.
/// This prevents path traversal in command names.
#[test]
fn command_with_path_separator_handled() {
    // `which` should reject commands with path separators
    // (they're either absolute paths already or invalid)
    let result = resolve_command_path("../../../bin/ls");
    // This should fail because `which` doesn't find commands with paths
    assert!(
        result.is_err(),
        "commands with path separators should not resolve via which"
    );
}

/// Security: Empty command name returns error.
#[test]
fn empty_command_returns_error() {
    let result = resolve_command_path("");
    assert!(result.is_err(), "empty command name should return an error");
}

/// Security: Command names with spaces are handled safely.
#[test]
fn command_with_spaces_handled() {
    let result = resolve_command_path("ls rm");
    // Commands with spaces should not resolve (not valid command names)
    assert!(
        result.is_err(),
        "command names with spaces should not resolve"
    );
}

// ---------------------------------------------------------------------------
// Caching behavior
// ---------------------------------------------------------------------------

/// Performance: Repeated resolution returns consistent results.
/// This verifies the cache returns the same path.
#[test]
fn repeated_resolution_consistent() {
    let path1 = resolve_command_path("ls").expect("first resolution should succeed");
    let path2 = resolve_command_path("ls").expect("second resolution should succeed");
    let path3 = resolve_command_path("ls").expect("third resolution should succeed");

    assert_eq!(path1, path2, "cached results must be consistent");
    assert_eq!(path2, path3, "cached results must be consistent");
}

/// Performance: Multiple different commands can be resolved.
#[test]
fn multiple_commands_resolvable() {
    let commands = ["ls", "sh", "cat", "echo", "pwd", "env"];
    let mut paths = Vec::new();

    for cmd in commands {
        if let Ok(path) = resolve_command_path(cmd) {
            paths.push((cmd, path));
        }
    }

    // At least some commands should resolve (ls, sh, cat are universal)
    assert!(
        paths.len() >= 3,
        "at least 3 common commands should resolve, got: {:?}",
        paths
    );

    // All resolved paths should be unique (different commands = different paths)
    let unique_paths: std::collections::HashSet<_> = paths.iter().map(|(_, p)| p).collect();
    assert_eq!(
        paths.len(),
        unique_paths.len(),
        "different commands should resolve to different paths"
    );
}

// ---------------------------------------------------------------------------
// Integration with allowlist enforcement
// ---------------------------------------------------------------------------

/// Integration: Resolved paths can be used for allowlist matching.
/// This verifies the path format is suitable for comparison.
#[test]
fn resolved_paths_usable_for_allowlist() {
    let path = resolve_command_path("ls").expect("ls should be resolvable");

    // The path should be usable for string comparison
    let path_str = path.to_string_lossy();
    assert!(
        !path_str.is_empty(),
        "path string representation should not be empty"
    );

    // Common patterns for where commands live
    let valid_locations = [
        "/bin/",
        "/usr/bin/",
        "/usr/local/bin/",
        "/sbin/",
        "/usr/sbin/",
    ];
    let in_valid_location = valid_locations.iter().any(|loc| path_str.contains(loc));

    assert!(
        in_valid_location,
        "ls should be in a standard bin directory, got: {}",
        path_str
    );
}

/// Integration: PathBuf allows further validation operations.
#[test]
fn pathbuf_operations_work() {
    let path = resolve_command_path("ls").expect("ls should be resolvable");

    // File existence check (the resolved command should exist)
    assert!(
        path.exists(),
        "resolved command path should exist on filesystem: {:?}",
        path
    );

    // File type check (should be a file, not directory)
    assert!(
        path.is_file() || path.is_symlink(),
        "resolved path should be a file or symlink: {:?}",
        path
    );

    // Parent directory check
    assert!(
        path.parent().is_some(),
        "resolved path should have a parent directory"
    );
}
