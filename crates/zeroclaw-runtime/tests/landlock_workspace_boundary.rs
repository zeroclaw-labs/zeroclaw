//! Integration test: verify the Landlock sandbox boundary by proving
//! that Landlock — not DAC, not missing paths, not permissions — causes
//! denial of outside operations while workspace operations succeed.
//!
//! nextest runs each test in a separate process, so this test can
//! fork a Landlock-restricted child without affecting other tests.

#![cfg(all(feature = "sandbox-landlock", target_os = "linux"))]

use std::path::Path;
use std::process::Command;
use zeroclaw_runtime::security::landlock::LandlockSandbox;
use zeroclaw_runtime::security::traits::Sandbox;

#[test]
fn landlock_workspace_boundary() {
    use tempfile::tempdir;

    // ── Setup: workspace and sandbox ──
    let workspace = tempdir().expect("failed to create temp directory");
    let ws = workspace.path();

    let sandbox = LandlockSandbox::with_workspace(Some(ws.to_path_buf()))
        .expect("landlock should succeed on linux with feature enabled");

    // ── Setup: unique outside target in /dev/shm ──
    // /dev/shm is NOT in the Landlock allow-list, so Landlock should
    // deny all handled operations there. A unique PID-based name avoids
    // collisions with parallel test runs.
    let outside_root = Path::new("/dev/shm");
    let probe = outside_root.join(".zeroclaw_boundary_probe");
    std::fs::write(&probe, b"").expect("/dev/shm must be writable — test environment is broken");
    let _ = std::fs::remove_file(&probe);

    let pid = std::process::id();
    let outside_file = outside_root.join(format!("zeroclaw_boundary_write_{pid}"));
    let outside_exec = outside_root.join(format!("zeroclaw_boundary_exec_{pid}"));

    // Clean up leftovers from previous runs (fail loudly, not silently).
    for path in [&outside_file, &outside_exec] {
        if path.exists() {
            std::fs::remove_file(path).expect("failed to clean up leftover from previous test run");
        }
    }

    // ── Baseline: prove the parent CAN perform every denied operation ──
    // The parent is unrestricted — restrict_self() runs only in the
    // forked child via pre_exec. If the parent cannot perform these
    // operations, the child's denial might be caused by DAC, missing
    // paths, or filesystem permissions — not Landlock.

    std::fs::write(&outside_file, "baseline")
        .expect("parent must be able to write to outside target — test env broken");
    assert_eq!(
        std::fs::read_to_string(&outside_file).expect("parent read must succeed"),
        "baseline",
    );

    // Copy a binary to the outside path for execution denial.
    // If /dev/shm is mounted noexec or the copy fails, skip the
    // execution denial test rather than producing a false failure.
    let parent_can_exec = {
        if std::fs::copy("/bin/true", &outside_exec).is_err() {
            false
        } else {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&outside_exec, std::fs::Permissions::from_mode(0o755));
            }
            Command::new(&outside_exec)
                .status()
                .map(|s| s.success())
                .unwrap_or(false)
        }
    };

    // Probe ABI support for Truncate (Landlock ABI 3+, kernel 6.2+).
    // On older ABIs, Truncate is not a handled right, so truncation
    // denial cannot be tested.
    let abi_supports_truncate = std::fs::read_to_string("/sys/kernel/security/landlock/abi")
        .ok()
        .and_then(|s| s.trim().parse::<u32>().ok())
        .is_some_and(|v| v >= 3);

    let has_truncate = Command::new("truncate")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    // ── Sandbox: child must be denied for outside operations ──
    // ── while workspace operations must still succeed ──
    //
    // The child is the same bash process but Landlock-restricted via
    // pre_exec. Since the parent proved it CAN perform these
    // operations, the child's failure proves Landlock — not DAC,
    // not missing paths, not permissions — caused the denial.

    let inside_file = ws.join("inside.txt");
    let inside_dir = ws.join("inside_dir");

    let mut script = String::new();
    script.push_str("set -e\n");

    // Positive: workspace operations must succeed.
    script.push_str(&format!("echo test > {}\n", inside_file.display()));
    script.push_str(&format!("test -f {}\n", inside_file.display()));
    script.push_str(&format!("rm {}\n", inside_file.display()));
    script.push_str(&format!("mkdir {}\n", inside_dir.display()));
    script.push_str(&format!("rmdir {}\n", inside_dir.display()));

    // Negative: outside write must be denied.
    script.push_str(&format!(
        "! echo bad > {} 2>/dev/null\n",
        outside_file.display(),
    ));

    // Negative: outside execution must be denied.
    if parent_can_exec {
        script.push_str(&format!("! {} 2>/dev/null\n", outside_exec.display()));
    }

    // Negative: outside truncation must be denied (ABI 3+ only).
    if abi_supports_truncate && has_truncate {
        script.push_str(&format!(
            "! truncate -s 0 {} 2>/dev/null\n",
            outside_file.display(),
        ));
    }

    let mut cmd = Command::new("bash");
    cmd.args(["-c", &script]);

    sandbox
        .wrap_command(&mut cmd)
        .expect("landlock should successfully wrap the command");

    let status = cmd
        .spawn()
        .expect("should spawn bash under landlock restrictions")
        .wait()
        .expect("should wait for bash to complete");

    assert!(
        status.success(),
        "boundary contract failed: workspace ops must succeed, \
         outside write/exec/truncate must be denied; exit status: {status}",
    );

    // The outside file must still contain "baseline" — proving neither
    // the write nor the truncate succeeded.
    assert_eq!(
        std::fs::read_to_string(&outside_file).unwrap_or_default(),
        "baseline",
        "sandboxed child must NOT be able to write to or truncate the outside target",
    );

    // Cleanup
    let _ = std::fs::remove_file(&outside_file);
    let _ = std::fs::remove_file(&outside_exec);
}
