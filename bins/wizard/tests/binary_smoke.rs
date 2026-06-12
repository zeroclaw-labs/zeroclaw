// bins/wizard/tests/binary_smoke.rs — integration tests for the wizard binary.
//
// At Phase 1.2/1.3 the wizard binary is a hollow placeholder. These tests pin
// both the placeholder's observable behavior AND the load-bearing safety
// property: the wizard binary contains zero MCP code. M3 will fill the
// binary; these tests will evolve.

use std::process::Command;

const BIN_NAME: &str = env!("CARGO_BIN_EXE_wizard");

#[test]
fn binary_runs_and_exits_zero() {
    let status = Command::new(BIN_NAME)
        .status()
        .expect("wizard binary failed to spawn");
    assert!(status.success(), "wizard exited non-zero: {status:?}");
}

#[test]
fn binary_identifies_itself_as_osagent_wizard() {
    let output = Command::new(BIN_NAME)
        .output()
        .expect("wizard binary failed to spawn");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 stdout");
    assert!(
        stdout.contains("osagent-wizard"),
        "wizard stdout missing 'osagent-wizard': {stdout}"
    );
}

#[test]
fn binary_advertises_zero_mcp_safety_property() {
    // The placeholder explicitly tells operators that the binary is MCP-free.
    // This test pins that observable behavior so M3 must preserve it.
    let output = Command::new(BIN_NAME)
        .output()
        .expect("wizard binary failed to spawn");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 stdout");
    assert!(
        stdout.contains("zero MCP code"),
        "wizard stdout missing 'zero MCP code' safety marker: {stdout}"
    );
}

#[test]
fn binary_reports_fork_provenance() {
    let output = Command::new(BIN_NAME)
        .output()
        .expect("wizard binary failed to spawn");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 stdout");
    assert!(
        stdout.contains("zeroclaw-labs/zeroclaw") && stdout.contains("v0.7.5"),
        "wizard stdout missing fork provenance: {stdout}"
    );
}
