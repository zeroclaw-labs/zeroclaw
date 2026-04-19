#![cfg(feature = "plugins-wasm")]

//! Security test: path traversal in arguments rejected.
//!
//! Task US-ZCL-54-5: Verifies acceptance criterion for US-ZCL-54:
//! > Path traversal in arguments rejected
//!
//! These tests exercise the `validate_path_traversal` function from the security
//! module to ensure that `..` path traversal sequences are blocked before they
//! could reach CLI command execution in the plugin system.

use zeroclaw::security::validate_path_traversal;

// ---------------------------------------------------------------------------
// Core acceptance criterion: path traversal patterns are rejected
// ---------------------------------------------------------------------------

/// AC: Standalone `..` is rejected.
#[test]
fn standalone_dotdot_rejected() {
    let result = validate_path_traversal(&[".."]);
    assert!(result.is_err(), "standalone '..' must be rejected");
    let err = result.unwrap_err();
    assert!(
        err.reason.contains("path traversal"),
        "error should mention path traversal"
    );
}

/// AC: Unix-style `../` prefix traversal is rejected.
/// Attack vector: `../../../etc/passwd` escapes to sensitive files.
#[test]
fn unix_prefix_traversal_rejected() {
    let result = validate_path_traversal(&["../etc/passwd"]);
    assert!(result.is_err(), "'../etc/passwd' must be rejected");

    let result = validate_path_traversal(&["../../etc/shadow"]);
    assert!(result.is_err(), "'../../etc/shadow' must be rejected");

    let result = validate_path_traversal(&["../../../root/.ssh/id_rsa"]);
    assert!(result.is_err(), "deep traversal must be rejected");
}

/// AC: Unix-style mid-path `/../` traversal is rejected.
/// Attack vector: `allowed/../../../etc/passwd` escapes from allowed directory.
#[test]
fn unix_midpath_traversal_rejected() {
    let result = validate_path_traversal(&["foo/../bar"]);
    assert!(result.is_err(), "'foo/../bar' must be rejected");

    let result = validate_path_traversal(&["a/b/../c/d"]);
    assert!(result.is_err(), "'a/b/../c/d' must be rejected");

    let result = validate_path_traversal(&["allowed/../../../etc/passwd"]);
    assert!(result.is_err(), "escape from allowed path must be rejected");
}

/// AC: Unix-style trailing `/..` traversal is rejected.
/// Attack vector: `dir/..` could be used to traverse upward.
#[test]
fn unix_trailing_traversal_rejected() {
    let result = validate_path_traversal(&["foo/.."]);
    assert!(result.is_err(), "'foo/..' must be rejected");

    let result = validate_path_traversal(&["path/to/dir/.."]);
    assert!(result.is_err(), "'path/to/dir/..' must be rejected");
}

/// AC: Windows-style `..\\` traversal is rejected.
/// Attack vector: `..\\..\\windows\\system32` on Windows systems.
#[test]
fn windows_traversal_rejected() {
    let result = validate_path_traversal(&["..\\windows\\system32"]);
    assert!(
        result.is_err(),
        "Windows '..\\\\' traversal must be rejected"
    );

    let result = validate_path_traversal(&["foo\\..\\bar"]);
    assert!(
        result.is_err(),
        "Windows mid-path traversal must be rejected"
    );

    let result = validate_path_traversal(&["foo\\.."]);
    assert!(
        result.is_err(),
        "Windows trailing traversal must be rejected"
    );

    let result = validate_path_traversal(&["..\\..\\..\\etc\\passwd"]);
    assert!(result.is_err(), "deep Windows traversal must be rejected");
}

/// AC: Multiple traversal sequences in one argument are rejected.
#[test]
fn multiple_traversals_rejected() {
    let result = validate_path_traversal(&["../../.."]);
    assert!(result.is_err(), "multiple traversals must be rejected");

    let result = validate_path_traversal(&["a/../b/../c"]);
    assert!(
        result.is_err(),
        "multiple mid-path traversals must be rejected"
    );

    let result = validate_path_traversal(&["../foo/../../bar"]);
    assert!(result.is_err(), "mixed traversal patterns must be rejected");
}

// ---------------------------------------------------------------------------
// Validation identifies the offending argument
// ---------------------------------------------------------------------------

/// The error should identify which argument contained the traversal.
#[test]
fn error_identifies_bad_argument() {
    // First argument is bad
    let result = validate_path_traversal(&["../secret", "safe.txt"]);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().argument,
        "../secret",
        "error should identify first bad argument"
    );

    // Second argument is bad
    let result = validate_path_traversal(&["safe.txt", "../secret"]);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().argument,
        "../secret",
        "error should identify second bad argument"
    );

    // Third argument is bad
    let result = validate_path_traversal(&["safe.txt", "also-safe", "bad/../path"]);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().argument,
        "bad/../path",
        "error should identify third bad argument"
    );
}

// ---------------------------------------------------------------------------
// Safe patterns are accepted (no false positives)
// ---------------------------------------------------------------------------

/// Normal file paths without traversal are accepted.
#[test]
fn safe_paths_accepted() {
    assert!(validate_path_traversal(&["file.txt"]).is_ok());
    assert!(validate_path_traversal(&["path/to/file"]).is_ok());
    assert!(validate_path_traversal(&["some/deep/nested/path"]).is_ok());
    assert!(validate_path_traversal(&["/absolute/path"]).is_ok());
    assert!(validate_path_traversal(&["relative/path/file.rs"]).is_ok());
}

/// CLI flags are accepted.
#[test]
fn cli_flags_accepted() {
    assert!(validate_path_traversal(&["-la"]).is_ok());
    assert!(validate_path_traversal(&["--flag=value"]).is_ok());
    assert!(validate_path_traversal(&["--output", "result.txt"]).is_ok());
    assert!(validate_path_traversal(&["-v", "--verbose"]).is_ok());
}

/// Empty arguments list is accepted.
#[test]
fn empty_args_accepted() {
    assert!(validate_path_traversal(&[]).is_ok());
}

/// Double dots that are NOT path traversal are accepted.
/// These are filenames/strings that happen to contain `..` but not as a path component.
#[test]
fn non_traversal_double_dots_accepted() {
    // Double dot in filename (not bounded by path separators)
    assert!(
        validate_path_traversal(&["file..txt"]).is_ok(),
        "double dot in filename should be allowed"
    );

    // Prefix `..` not followed by separator
    assert!(
        validate_path_traversal(&["..suffix"]).is_ok(),
        "'..suffix' is not traversal (no separator after)"
    );

    // Suffix `..` not preceded by separator
    assert!(
        validate_path_traversal(&["prefix.."]).is_ok(),
        "'prefix..' is not traversal (no separator before)"
    );

    // `..` embedded in the middle of a name
    assert!(
        validate_path_traversal(&["a..b"]).is_ok(),
        "'a..b' is not traversal"
    );

    // Triple dot (not the same as `..`)
    assert!(
        validate_path_traversal(&["..."]).is_ok(),
        "triple dot should be allowed"
    );

    // Hidden files (single dot)
    assert!(
        validate_path_traversal(&[".hidden"]).is_ok(),
        "hidden file should be allowed"
    );

    // Multiple dots in extension
    assert!(
        validate_path_traversal(&["file.tar.gz"]).is_ok(),
        "dotted extensions should be allowed"
    );
}

// ---------------------------------------------------------------------------
// Real-world attack patterns
// ---------------------------------------------------------------------------

/// Common path traversal attack patterns used in the wild.
#[test]
fn real_world_traversal_attacks_rejected() {
    // Classic /etc/passwd access
    assert!(validate_path_traversal(&["../../../etc/passwd"]).is_err());
    assert!(validate_path_traversal(&["....//....//....//etc/passwd"]).is_ok()); // not valid traversal

    // SSH key theft
    assert!(validate_path_traversal(&["../../../home/user/.ssh/id_rsa"]).is_err());
    assert!(validate_path_traversal(&["..\\..\\..\\Users\\Admin\\.ssh\\id_rsa"]).is_err());

    // Config file access
    assert!(validate_path_traversal(&["../../../etc/shadow"]).is_err());
    assert!(validate_path_traversal(&["../../.env"]).is_err());

    // Application escape
    assert!(validate_path_traversal(&["uploads/../config/database.yml"]).is_err());
    assert!(validate_path_traversal(&["static/../../../app/secrets.json"]).is_err());

    // Log file access
    assert!(validate_path_traversal(&["../../var/log/auth.log"]).is_err());
}

/// URL-encoded traversal should be handled at a different layer (these test raw strings).
#[test]
fn url_encoded_traversal_passes_through() {
    // URL encoding is NOT decoded by validate_path_traversal
    // (decoding should happen before validation if needed)
    assert!(
        validate_path_traversal(&["%2e%2e%2fetc%2fpasswd"]).is_ok(),
        "URL-encoded traversal is not decoded (handled elsewhere)"
    );
    assert!(
        validate_path_traversal(&["..%2f..%2f"]).is_ok(),
        "partial URL encoding not decoded"
    );
}

// ---------------------------------------------------------------------------
// Multiple arguments validation
// ---------------------------------------------------------------------------

/// All arguments in a list are validated.
#[test]
fn all_arguments_validated() {
    // Many safe arguments
    assert!(validate_path_traversal(&["file1.txt", "file2.txt", "path/to/file3.txt"]).is_ok());

    // One bad argument among many
    let result = validate_path_traversal(&["safe1.txt", "safe2.txt", "../bad.txt", "safe3.txt"]);
    assert!(result.is_err());
    assert_eq!(result.unwrap_err().argument, "../bad.txt");
}

/// Validation stops at first bad argument.
#[test]
fn validation_stops_at_first_bad() {
    let result = validate_path_traversal(&["../first", "../second"]);
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().argument,
        "../first",
        "should report first bad argument"
    );
}
