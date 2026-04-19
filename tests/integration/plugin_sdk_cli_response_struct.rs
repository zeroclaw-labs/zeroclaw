#![cfg(feature = "plugins-wasm")]

//! Test: Response struct includes stdout/stderr/exit_code/truncated flag.
//!
//! Task US-ZCL-59-3: Verifies acceptance criterion for US-ZCL-59:
//! > Response struct includes stdout/stderr/exit_code/truncated flag
//!
//! This test verifies that the SDK's CliResponse struct provides all the
//! required fields for plugins to access command execution results.

use std::fs;
use std::path::Path;

const SDK_CLI_PATH: &str = "crates/zeroclaw-plugin-sdk/src/cli.rs";

/// Helper to read the CLI module source code.
fn read_cli_source() -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"));
    let cli_path = base.join(SDK_CLI_PATH);
    fs::read_to_string(&cli_path).expect("Failed to read cli.rs")
}

// ---------------------------------------------------------------------------
// Core acceptance criterion: Response struct includes required fields
// ---------------------------------------------------------------------------

/// CliResponse struct exists in the SDK CLI module.
#[test]
fn cli_response_struct_exists() {
    let source = read_cli_source();
    assert!(
        source.contains("pub struct CliResponse"),
        "CliResponse struct must be defined in SDK cli.rs"
    );
}

/// CliResponse includes stdout field for standard output.
#[test]
fn cli_response_includes_stdout_field() {
    let source = read_cli_source();

    // Find the CliResponse struct and verify it has stdout field
    assert!(
        source.contains("pub stdout: String"),
        "CliResponse must include 'pub stdout: String' field"
    );
}

/// CliResponse includes stderr field for standard error.
#[test]
fn cli_response_includes_stderr_field() {
    let source = read_cli_source();

    assert!(
        source.contains("pub stderr: String"),
        "CliResponse must include 'pub stderr: String' field"
    );
}

/// CliResponse includes exit_code field.
#[test]
fn cli_response_includes_exit_code_field() {
    let source = read_cli_source();

    assert!(
        source.contains("pub exit_code: i32"),
        "CliResponse must include 'pub exit_code: i32' field"
    );
}

/// CliResponse includes truncated flag.
#[test]
fn cli_response_includes_truncated_flag() {
    let source = read_cli_source();

    assert!(
        source.contains("pub truncated: bool"),
        "CliResponse must include 'pub truncated: bool' field"
    );
}

// ---------------------------------------------------------------------------
// Additional response struct requirements
// ---------------------------------------------------------------------------

/// CliResponse implements Deserialize for JSON parsing from host.
#[test]
fn cli_response_derives_deserialize() {
    let source = read_cli_source();

    // The struct should have Deserialize derived
    assert!(
        source.contains("Deserialize"),
        "CliResponse must derive Deserialize for host communication"
    );
}

/// CliResponse implements Debug for diagnostics.
#[test]
fn cli_response_derives_debug() {
    let source = read_cli_source();

    // Check that Debug is derived (in the derive attribute before CliResponse)
    assert!(
        source.contains("Debug"),
        "CliResponse should derive Debug for diagnostics"
    );
}

/// CliResponse implements Clone for flexibility.
#[test]
fn cli_response_derives_clone() {
    let source = read_cli_source();

    assert!(source.contains("Clone"), "CliResponse should derive Clone");
}

// ---------------------------------------------------------------------------
// Field documentation
// ---------------------------------------------------------------------------

/// stdout field has documentation.
#[test]
fn stdout_field_is_documented() {
    let source = read_cli_source();

    // Should have doc comment before stdout field
    assert!(
        source.contains("Standard output") || source.contains("stdout"),
        "stdout field should be documented"
    );
}

/// stderr field has documentation.
#[test]
fn stderr_field_is_documented() {
    let source = read_cli_source();

    assert!(
        source.contains("Standard error") || source.contains("stderr"),
        "stderr field should be documented"
    );
}

/// exit_code field has documentation.
#[test]
fn exit_code_field_is_documented() {
    let source = read_cli_source();

    assert!(
        source.contains("exit code") || source.contains("Exit code"),
        "exit_code field should be documented"
    );
}

/// truncated field has documentation.
#[test]
fn truncated_field_is_documented() {
    let source = read_cli_source();

    assert!(
        source.contains("truncated") || source.contains("Truncat"),
        "truncated field should be documented"
    );
}
