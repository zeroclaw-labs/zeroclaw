//! Integration test: verify that `restrict_self()` failure inside
//! `pre_exec` propagates through `spawn()` as `Err`.
//!
//! When all 16 Landlock layers are exhausted, the 17th `restrict_self()`
//! call in the child's `pre_exec` hook should fail. After the fix that
//! converts `restrict_self()` failure into an `io::Error` (instead of
//! `panic!`), `spawn()` must return `Err` rather than producing an
//! apparently-spawned child that later dies with `SIGABRT`.
//!
//! nextest runs each test in a separate process, so exhausting all 16
//! Landlock layers in this test does not affect other tests.

#![cfg(all(feature = "sandbox-landlock", target_os = "linux"))]

use landlock::{AccessFs, PathBeneath, PathFd, Ruleset, RulesetAttr, RulesetCreatedAttr};
use std::path::Path;
use std::process::Command;
use zeroclaw_runtime::security::landlock::LandlockSandbox;
use zeroclaw_runtime::security::traits::Sandbox;

/// `E2BIG` on Linux (errno 7) — returned by `landlock_restrict_self(2)` when
/// `LANDLOCK_MAX_NUM_LAYERS` (16) is exceeded. Defined inline because `libc`
/// is a regular (non-dev) dependency and thus unavailable in integration tests.
const E2BIG: i32 = 7;

#[test]
fn landlock_spawn_failure_returns_err() {
    // Exhaust all 16 Landlock layers (LANDLOCK_MAX_NUM_LAYERS).
    // Each layer must allow the paths we need for the subsequent
    // wrap_command + spawn, otherwise the test would lock itself out
    // before it can build the final ruleset.
    for _ in 0..16 {
        let mut ruleset = Ruleset::default()
            .handle_access(AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ReadDir)
            .and_then(|r| r.create())
            .expect("Landlock not available. Landlock is required to run this test.");

        // Add permissive rules for each path. `add_rule` consumes `self`,
        // so we thread ownership through an Option.
        for path in ["/tmp", "/usr", "/bin", "/lib", "/etc"] {
            if let Ok(fd) = PathFd::new(Path::new(path)) {
                ruleset = ruleset
                    .add_rule(PathBeneath::new(
                        fd,
                        AccessFs::ReadFile | AccessFs::WriteFile | AccessFs::ReadDir,
                    ))
                    .expect("Should be able to add rule");
            } else {
                eprintln!(
                    "Unable to add {} to ruleset. Test may be affected. Caveat emptor",
                    path
                );
            }
        }

        ruleset
            .restrict_self()
            .expect("Should be able to exhaust layers");
    }

    // Now create a LandlockSandbox and try to wrap_command + spawn.
    // The child's pre_exec will call restrict_self() which should
    // fail because all 16 layers are already consumed.
    let sandbox =
        LandlockSandbox::new().expect("How did you get here? I checked it before doing this");

    let mut cmd = Command::new("true");
    sandbox
        .wrap_command(&mut cmd)
        .expect("wrap_command must succeed");

    match cmd.spawn() {
        Err(e) => {
            // The failure must be the Landlock layer-exhaustion errno
            // (E2BIG), not an unrelated process-creation error.
            assert_eq!(
                e.raw_os_error(),
                Some(E2BIG),
                "spawn() must fail with E2BIG (exceeded LANDLOCK_MAX_NUM_LAYERS), \
                 but got: {e}"
            );
        }
        Ok(mut child) => {
            let _ = child.wait();
            panic!("spawn() must return Err after layer exhaustion, but returned Ok");
        }
    }
}
