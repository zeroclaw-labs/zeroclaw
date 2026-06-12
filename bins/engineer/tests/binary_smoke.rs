// bins/engineer/tests/binary_smoke.rs — integration tests for the engineer binary.
//
// At Phase 1.2/1.3 the engineer binary is a hollow placeholder. These tests pin
// the placeholder's observable behavior so we notice if a future change breaks
// it accidentally. M2 will replace the placeholder; tests will evolve with it.

use std::process::Command;

const BIN_NAME: &str = env!("CARGO_BIN_EXE_engineer");

#[test]
fn binary_runs_and_exits_zero() {
    let status = Command::new(BIN_NAME)
        .status()
        .expect("engineer binary failed to spawn");
    assert!(status.success(), "engineer exited non-zero: {status:?}");
}

#[test]
fn binary_identifies_itself_as_osagent_engineer() {
    let output = Command::new(BIN_NAME)
        .output()
        .expect("engineer binary failed to spawn");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 stdout");
    assert!(
        stdout.contains("osagent-engineer"),
        "engineer stdout missing 'osagent-engineer': {stdout}"
    );
    assert!(
        stdout.contains("engineer"),
        "engineer stdout missing 'engineer' identifier: {stdout}"
    );
}

#[test]
fn binary_reports_fork_provenance() {
    let output = Command::new(BIN_NAME)
        .output()
        .expect("engineer binary failed to spawn");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 stdout");
    assert!(
        stdout.contains("zeroclaw-labs/zeroclaw") && stdout.contains("v0.7.5"),
        "engineer stdout missing fork provenance: {stdout}"
    );
}
