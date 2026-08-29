//! Integration test: verify the Landlock sandbox boundary by proving
//! that Landlock — not DAC, not missing paths, not permissions — causes
//! denial of outside operations while workspace operations succeed.
//!
//! nextest runs each test in a separate process, so this test can
//! fork a Landlock-restricted child without affecting other tests.

#![cfg(all(feature = "sandbox-landlock", target_os = "linux"))]

use std::path::{Path, PathBuf};
use std::process::Command;
use zeroclaw_runtime::security::landlock::LandlockSandbox;
use zeroclaw_runtime::security::traits::Sandbox;

/// Query the Landlock ABI version via the documented kernel API:
/// `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`.
///
/// This is the kernel's own method for querying Landlock ABI support,
/// as [documented](https://www.kernel.org/doc/html/latest/userspace-api/landlock.html)
/// in the Linux kernel userspace API. It returns the highest supported
/// ABI version (1+), or 0 if Landlock is not available.
///
/// **Why not `/sys/kernel/security/landlock/abi`?** That sysfs file is
/// not always present (e.g., on hosts where `securityfs` is not mounted)
/// and is not the documented ABI query mechanism. The syscall is.
///
/// **Why not the landlock crate's `ABI::current()`?** That method is
/// `pub(crate)` and only accessible from within the crate. The crate
/// internally calls this same syscall.
fn landlock_abi_version() -> u32 {
    // LANDLOCK_CREATE_RULESET_VERSION = 1 (bit 0 of the flags argument).
    // When passed with NULL attrs + size 0, the syscall returns the
    // highest supported ABI version instead of creating a ruleset.
    const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1;

    // SAFETY: this is the kernel-documented version-query form: a null attrs
    // pointer with size zero, plus the VERSION flag. No memory is read or
    // written through the null pointer.
    let ret = unsafe {
        libc::syscall(
            libc::SYS_landlock_create_ruleset,
            std::ptr::null::<libc::c_void>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    if ret < 0 { 0 } else { ret as u32 }
}

/// Find a writable AND executable directory outside the Landlock allowlist.
///
/// The Landlock allowlist in `build_ruleset()` includes:
/// `/tmp` (PathBeneath), `/usr` (PathBeneath), `/bin` (PathBeneath),
/// `/lib`, `/lib64`, `/dev/null`, and the workspace directory.
///
/// We need a directory that is NOT beneath any of these paths so that
/// Landlock denies all operations there. Candidates:
/// 1. `/dev/shm` — tmpfs, usually writable. May be mounted `noexec`.
/// 2. `/var/tmp` — persistent temp dir, not beneath the allowlist.
///
/// **Both write and execution must work.** A `noexec` mount such as
/// `/dev/shm` on some hosts would make execution baselines impossible,
/// and the execution denial assertion would be vacuous. We therefore
/// reject candidates that do not support execution and continue to the
/// next one, rather than returning a directory where execution denial
/// cannot be proven.
fn find_outside_dir() -> PathBuf {
    let candidates: &[&str] = &["/dev/shm", "/var/tmp"];
    let pid = std::process::id();

    for &dir in candidates {
        let p = Path::new(dir);
        if !p.is_dir() {
            continue;
        }

        // Write test — confirms we can create files here.
        let write_probe = p.join(format!(".zc_write_probe_{pid}"));
        if std::fs::write(&write_probe, b"").is_err() {
            continue;
        }

        // Execution test — copy /bin/true, set +x, and run it.
        // If the filesystem is mounted `noexec`, this fails and we
        // continue to the next candidate instead of returning a
        // directory where execution denial cannot be proven.
        let exec_probe = p.join(format!(".zc_exec_probe_{pid}"));
        let exec_ok = {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::copy("/bin/true", &exec_probe).is_ok()
                    && std::fs::set_permissions(&exec_probe, std::fs::Permissions::from_mode(0o755))
                        .is_ok()
                    && Command::new(&exec_probe)
                        .status()
                        .map(|s| s.success())
                        .unwrap_or(false)
            }
            #[cfg(not(unix))]
            {
                let _ = &exec_probe;
                false
            }
        };

        let _ = std::fs::remove_file(&write_probe);
        let _ = std::fs::remove_file(&exec_probe);

        if exec_ok {
            return p.to_path_buf();
        }
    }

    panic!(
        "No writable AND executable directory outside the Landlock allowlist found. \
         Tried: /dev/shm, /var/tmp. Both write and execution must be supported \
         (noexec mounts are not suitable). Test environment is broken."
    );
}

#[test]
fn landlock_workspace_boundary() {
    use tempfile::tempdir;

    // ── Setup: workspace and sandbox ──
    let workspace = tempdir().expect("failed to create temp directory");
    let ws = workspace.path();

    let sandbox = LandlockSandbox::with_workspace(Some(ws.to_path_buf()))
        .expect("landlock should succeed on linux with feature enabled");

    // ── Query ABI using the documented kernel API ──
    // Truncate was introduced in ABI V3 (kernel 6.2+). On older kernels,
    // Truncate is not a handled right — the kernel ignores it and the
    // operation is unconfined. We only test truncation denial on ABI 3+.
    //
    // We use the raw syscall (not sysfs) because the sysfs file may not
    // exist on all configurations, and the syscall is the documented API.
    let abi = landlock_abi_version();
    assert!(abi >= 1, "Landlock ABI must be >= 1, got {abi}");
    let abi_supports_truncate = abi >= 3;

    // ── Find outside target directory ──
    // The outside directory must NOT be beneath any Landlock-allowlisted
    // path (/tmp, /usr, /bin, /lib, /lib64, /dev/null, workspace).
    let outside_dir = find_outside_dir();
    let pid = std::process::id();

    // Each denied operation gets its own file so that denial is
    // independently provable — the failure of one assertion does not
    // mask the failure of another.
    //
    // 1. `outside_write` — for non-truncating write denial (WriteFile).
    // 2. `outside_exec`  — for execution denial (Execute).
    // 3. `outside_trunc` — for truncation denial (Truncate, ABI 3+ only).
    let outside_write = outside_dir.join(format!("zc_write_{pid}"));
    let outside_exec = outside_dir.join(format!("zc_exec_{pid}"));
    let outside_trunc = outside_dir.join(format!("zc_trunc_{pid}"));

    // Clean up leftovers from previous runs (fail loudly, not silently).
    for path in [&outside_write, &outside_exec, &outside_trunc] {
        if path.exists() {
            std::fs::remove_file(path).expect("failed to clean up leftover from previous test run");
        }
    }

    // ── Baselines: prove the parent CAN perform each operation ──
    //
    // The parent process is unrestricted — `restrict_self()` runs only
    // in the forked child via `pre_exec`. If the parent cannot perform
    // an operation, the child's denial might be caused by DAC, missing
    // paths, or filesystem permissions — not Landlock. Thus, every
    // denial assertion must be preceded by a successful parent baseline.

    // Baseline 1: non-truncating write (append mode).
    //
    // `printf ... >> file` opens the file with `O_WRONLY | O_CREAT | O_APPEND`
    // — NO `O_TRUNC`. This exercises `WriteFile` independently of `Truncate`.
    //
    // This is the key difference from `echo bad > file`, which uses
    // `O_WRONLY | O_CREAT | O_TRUNC` and thus conflates `WriteFile`
    // and `Truncate` denial.
    std::fs::write(&outside_write, "baseline")
        .expect("parent must be able to write to outside target — test env broken");
    assert_eq!(
        std::fs::read_to_string(&outside_write).expect("parent read must succeed"),
        "baseline",
    );

    // Baseline 2: execution from the outside path.
    //
    // The parent copies `/bin/true` to the outside path and executes
    // it. If the filesystem is mounted `noexec`, execution will fail
    // — in that case, we FAIL the test rather than silently skipping
    // the execution denial assertion.
    //
    // Reviewer concern: the original test silently omitted the execution
    // denial when `/dev/shm` was `noexec`, meaning the execution boundary
    // was never exercised. We now try multiple directories and fail loud
    // if none of them support execution.
    std::fs::copy("/bin/true", &outside_exec)
        .expect("parent must be able to copy /bin/true to outside path");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&outside_exec, std::fs::Permissions::from_mode(0o755))
            .expect("parent must be able to set executable permissions");
    }
    assert!(
        Command::new(&outside_exec)
            .status()
            .expect("parent must be able to spawn outside binary")
            .success(),
        "parent must be able to execute a binary from {} — \
         the filesystem must not be mounted noexec. \
         If this is CI, configure an exec-capable outside target.",
        outside_dir.display(),
    );

    // Baseline 3: truncation (ABI 3+ only).
    //
    // `truncate -s 0 file` calls `truncate(2)`, which requires only
    // `Truncate` — NOT `WriteFile`. This independently exercises the
    // `Truncate` boundary, separate from `WriteFile`.
    //
    // On ABI < 3, `Truncate` is not a handled right, so there is no
    // boundary to test — we skip with an explicit assert that documents
    // the ABI level, rather than silently omitting the test.
    if abi_supports_truncate {
        std::fs::write(&outside_trunc, "baseline_trunc")
            .expect("parent must be able to write truncation target");

        // The `truncate` command must be available — if not, fail
        // rather than silently skipping the truncation assertion.
        assert!(
            Command::new("truncate")
                .arg("--version")
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false),
            "`truncate` command must be available to test truncation denial on ABI {} ({}). \
             Install coreutils or equivalent.",
            abi,
            if abi_supports_truncate {
                "supports Truncate"
            } else {
                "does not support Truncate"
            },
        );

        // Parent baseline: truncate must succeed.
        let trunc_status = Command::new("truncate")
            .args(["-s", "0"])
            .arg(&outside_trunc)
            .status()
            .expect("parent truncate must be able to spawn");
        assert!(
            trunc_status.success(),
            "parent must be able to truncate outside target — test env broken",
        );
        assert_eq!(
            std::fs::metadata(&outside_trunc)
                .expect("parent must be able to stat truncation target")
                .len(),
            0,
            "parent truncate must have zeroed the file",
        );
        // Restore content for the child denial assertion.
        std::fs::write(&outside_trunc, "baseline_trunc")
            .expect("parent must restore truncation target content");
    } else {
        eprintln!(
            "Landlock ABI {} < 3: Truncate is not a handled right. \
             Skipping truncation denial test (expected on kernel < 6.2).",
            abi,
        );
    }

    // ── Sandbox: child must be denied for outside operations ──
    // ── while workspace operations must still succeed ──
    //
    // The child runs a bash script under Landlock restrictions.
    // Since the parent proved it CAN perform each operation above,
    // the child's failure proves Landlock — not DAC, not missing
    // paths, not filesystem permissions — caused the denial.

    let inside_file = ws.join("inside.txt");
    let inside_dir = ws.join("inside_dir");
    let inside_exec = ws.join("inside_exec");

    // Parent copies /bin/true into the workspace so the child can
    // exercise its granted Execute right on a workspace binary.
    std::fs::copy("/bin/true", &inside_exec)
        .expect("parent must be able to copy /bin/true into workspace");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&inside_exec, std::fs::Permissions::from_mode(0o755))
            .expect("parent must set exec permissions on workspace test binary");
    }

    let mut script = String::new();
    script.push_str("set -e\n");

    // ── Positive: /dev/null must be accessible ──
    //
    // Negative probes redirect stderr to /dev/null. If /dev/null is
    // blocked, the redirect fails before the probed operation, and
    // bash's exit-status logic could turn that unrelated failure into
    // an apparent success. We prove /dev/null is accessible first so
    // that subsequent redirect failures are attributable to the
    // probed operation, not to a blocked /dev/null.
    script.push_str(
        "if ! echo test > /dev/null; then \
            echo 'FAIL: child cannot access /dev/null — stderr redirects are unreliable' >&2; \
            exit 1; \
         fi\n",
    );

    // ── Positive: workspace WriteFile ──
    script.push_str(&format!("echo test > {}\n", inside_file.display()));

    // ── Positive: workspace ReadFile ──
    //
    // Read back the file we just wrote and verify its content.
    // This exercises the ReadFile grant on the workspace.
    script.push_str(&format!(
        "if ! content=$(cat {}); then \
            echo 'FAIL: workspace ReadFile failed' >&2; \
            exit 1; \
         fi\n",
        inside_file.display(),
    ));
    script.push_str(
        "if [ \"$content\" != 'test' ]; then \
            echo \"FAIL: workspace ReadFile returned '$content' instead of 'test'\" >&2; \
            exit 1; \
         fi\n",
    );

    // ── Positive: workspace stat and directory operations ──
    script.push_str(&format!("test -f {}\n", inside_file.display()));
    script.push_str(&format!("rm {}\n", inside_file.display()));
    script.push_str(&format!("mkdir {}\n", inside_dir.display()));
    script.push_str(&format!("rmdir {}\n", inside_dir.display()));

    // ── Positive: workspace Execute ──
    //
    // Execute the binary the parent copied into the workspace.
    // This exercises the Execute grant on the workspace path.
    script.push_str(&format!(
        "if ! {}; then \
            echo 'FAIL: workspace Execute failed' >&2; \
            exit 1; \
         fi\n",
        inside_exec.display(),
    ));
    script.push_str(&format!("rm {}\n", inside_exec.display()));

    // ── Negative: outside write (append) must be denied ──
    //
    // `printf 'x' >> file` uses `O_APPEND` — no `O_TRUNC`. This
    // independently proves `WriteFile` is restricted.
    //
    // We use an explicit `if cmd; then exit 1; fi` instead of
    // `! cmd` because bash exempts commands negated with `!` from
    // `set -e` (errexit), so an unexpected success would not
    // terminate the script. The `if` form exits with failure if
    // the operation unexpectedly succeeds.
    script.push_str(&format!(
        "if printf 'x' >> {} 2>/dev/null; then \
            echo 'FAIL: outside write should have been denied' >&2; \
            exit 1; \
         fi\n",
        outside_write.display(),
    ));

    // ── Negative: outside execution must be denied ──
    //
    // The parent confirmed this binary is executable on this
    // filesystem. The child's failure to execute it proves the
    // `Execute` boundary is enforced.
    script.push_str(&format!(
        "if {} 2>/dev/null; then \
            echo 'FAIL: outside exec should have been denied' >&2; \
            exit 1; \
         fi\n",
        outside_exec.display(),
    ));

    // ── Negative: outside truncation must be denied (ABI 3+ only) ──
    //
    // `truncate -s 0 file` calls `truncate(2)`, which requires only
    // `Truncate` — NOT `WriteFile`. This independently proves the
    // `Truncate` boundary is enforced.
    if abi_supports_truncate {
        script.push_str(&format!(
            "if truncate -s 0 {} 2>/dev/null; then \
                echo 'FAIL: outside truncation should have been denied' >&2; \
                exit 1; \
             fi\n",
            outside_trunc.display(),
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
        "boundary contract failed: workspace ops (write/read/exec) must succeed, \
         outside write/exec/truncate must be denied; exit status: {status}",
    );

    // ── Verify outside files are unchanged ──
    //
    // Each outside file must still contain its baseline content,
    // proving the sandboxed child could NOT perform the operation.

    // Write file: must still contain "baseline".
    // The non-truncating append must NOT have succeeded.
    assert_eq!(
        std::fs::read_to_string(&outside_write).unwrap_or_default(),
        "baseline",
        "sandboxed child must NOT be able to write (append) to the outside target",
    );

    if abi_supports_truncate {
        // Truncate file: must still contain "baseline_trunc".
        // The truncation must NOT have succeeded.
        assert_eq!(
            std::fs::read_to_string(&outside_trunc).unwrap_or_default(),
            "baseline_trunc",
            "sandboxed child must NOT be able to truncate the outside target",
        );
    }

    // Cleanup
    let _ = std::fs::remove_file(&outside_write);
    let _ = std::fs::remove_file(&outside_exec);
    if abi_supports_truncate {
        let _ = std::fs::remove_file(&outside_trunc);
    }
}
