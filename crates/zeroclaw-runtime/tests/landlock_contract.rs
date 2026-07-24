//! Integration test: verify all sides of the Landlock sandboxing
//! contract in a single isolated process.
//!
//! nextest runs each test in a separate process, so this test can
//! verify all contract sides directly without affecting other tests.
//!
//! Contract sides verified:
//!   1. Baseline parent access (reads /etc/passwd before sandboxing)
//!   2. Child allowed path (reads canary from /tmp — must succeed)
//!   3. Child denied path (reads /etc/passwd — must fail)
//!   4. Post-parent access (reads /etc/passwd after spawning children)

#![cfg(all(feature = "sandbox-landlock", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use zeroclaw_runtime::security::landlock::LandlockSandbox;
use zeroclaw_runtime::security::traits::Sandbox;

#[test]
fn landlock_contract() {
    let sentinel = Path::new("/etc/passwd");

    // Test host is able to read sentinel before doing test
    assert!(
        std::fs::read_to_string(sentinel).is_ok(),
        "test framework should be able to read sentinel to test properly"
    );

    // Create sandbox
    let sandbox = LandlockSandbox::new()
        .expect("Landlock not available. Landlock is required to run this test.");

    // Write canary to /tmp (allow-listed for read+write)
    let canary_path = std::env::temp_dir().join("landlock_contract_canary.txt");
    std::fs::write(&canary_path, "canary").expect("must write canary");

    // Side 2: child allowed path — `cat /tmp/canary` must succeed
    let mut cmd_allowed = Command::new("cat");
    cmd_allowed.arg(&canary_path);
    sandbox
        .wrap_command(&mut cmd_allowed)
        .expect("wrap_command must succeed");
    let output = cmd_allowed.output().expect("child must execute");

    assert!(
        output.status.success(),
        "child must access allowed path (/tmp canary). \
         exit={:?} stdout={} stderr={}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "canary",
        "child must produce canary contents"
    );

    // Side 3: child denied path — `cat /etc/passwd` must fail
    let mut cmd_denied = Command::new("cat");
    cmd_denied.arg("/etc/passwd");
    sandbox
        .wrap_command(&mut cmd_denied)
        .expect("wrap_command must succeed");
    let output = cmd_denied.output().expect("child must execute");

    assert!(
        !output.status.success(),
        "child must be denied access to /etc/passwd. \
         exit={:?} stdout_len={}",
        output.status.code(),
        output.stdout.len(),
    );
    assert!(
        output.stdout.is_empty(),
        "child must not produce stdout when Landlock blocks the read. \
         stdout_len={}",
        output.stdout.len(),
    );

    // Side 4: post-parent access — test process must still read /etc/passwd
    assert!(
        std::fs::read_to_string(sentinel).is_ok(),
        "parent must still have access after spawning children"
    );

    // Cleanup
    let _ = std::fs::remove_file(&canary_path);
}
