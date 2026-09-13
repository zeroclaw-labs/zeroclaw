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
    // The probe filenames must be unique per *call*, not merely per process.
    // Under `cargo test` the tests in this binary run as threads in one
    // process, so a pid-only name makes concurrent callers share probe paths
    // and delete each other's files mid-check — the function then reports
    // "test environment is broken" for a collision it caused itself. (nextest,
    // which CI uses, gives each test its own process and hides this.)
    static PROBE_SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let pid = format!(
        "{}_{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );

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

/// Create a unique, empty directory outside the generic Landlock allow-list.
///
/// Extra-root tiers must be probed outside `/tmp`. The generic allow-list
/// already grants `/tmp` `ReadFile | WriteFile | Truncate`, so a `tempdir()`
/// root inherits those rights and a tier granting nothing at all would still
/// look like it worked — while a tier's *denials* could equally be caused by
/// rights `/tmp` never had (it has no `MakeReg`, so "cannot create a file"
/// proves nothing about the tier). Only a root outside that set isolates the
/// tier's own permission bits.
fn outside_root(tag: &str) -> PathBuf {
    let dir = find_outside_dir().join(format!("zc_root_{tag}_{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("failed to clear leftover outside root");
    }
    std::fs::create_dir_all(&dir).expect("failed to create outside root");
    dir
}

/// Serializes the tests in this file.
///
/// These tests fork and exec repeatedly. `cargo test` runs them as threads in a
/// single process, so one thread's `Command::spawn` can inherit a writable fd
/// that another thread is about to execute, and the `execve` then fails with
/// `ETXTBSY` ("Text file busy") — a harness artifact, not a sandbox defect. The
/// module already assumes one process per test (nextest, which CI uses); this
/// lock gives plain `cargo test` the same guarantee. The tests take ~10ms
/// total, so serializing them costs nothing.
static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the serialization lock, ignoring poisoning: a panic in one test must
/// surface as that test's own failure, not as a cascade of poisoned-lock
/// failures in the others.
fn serialize_test() -> std::sync::MutexGuard<'static, ()> {
    TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Run `script` under `sandbox` and report whether it exited zero.
fn run_sandboxed(sandbox: &LandlockSandbox, script: &str) -> bool {
    let mut cmd = Command::new("bash");
    cmd.args(["-c", script]);
    sandbox
        .wrap_command(&mut cmd)
        .expect("landlock should successfully wrap the command");
    cmd.spawn()
        .expect("should spawn bash under landlock restrictions")
        .wait()
        .expect("should wait for bash to complete")
        .success()
}

#[test]
fn landlock_workspace_boundary() {
    let _serialized = serialize_test();
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

/// Regression test: `LandlockSandbox::with_roots` must grant kernel-level
/// access to `SecurityPolicy`'s extra allowed-roots tiers, not just the
/// primary workspace. Before this fix, `LandlockSandbox` only ever saw
/// `workspace_dir`, so a directory the application-layer policy permitted
/// via `allowed_roots` (e.g. `[autonomy].allowed_roots` or a cross-agent
/// grant) was still rejected by the kernel — Landlock silently became
/// *more* restrictive than the configured policy.
#[test]
fn landlock_with_roots_grants_extra_allowed_root_access() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let extra_rw = outside_root("rw");
    let extra_ro = outside_root("ro");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        vec![extra_rw.clone()],
        vec![extra_ro.clone()],
        Vec::new(),
    )
    .expect("landlock should succeed on linux with feature enabled");

    let rw_file = extra_rw.join("rw.txt");
    let ro_file = extra_ro.join("ro.txt");
    let ro_write_target = extra_ro.join("should_not_be_created.txt");

    // Parent seeds the read-only root's content before the child runs.
    std::fs::write(&ro_file, "seed").expect("parent must seed read-only root content");

    let mut script = String::new();
    script.push_str("set -e\n");

    // Positive: the rw extra root must be writable and readable.
    script.push_str(&format!("echo test > {}\n", rw_file.display()));
    script.push_str(&format!(
        "if [ \"$(cat {})\" != 'test' ]; then \
            echo 'FAIL: extra read-write root ReadFile/WriteFile failed' >&2; \
            exit 1; \
         fi\n",
        rw_file.display(),
    ));

    // Positive: the read-only extra root must be readable.
    script.push_str(&format!(
        "if [ \"$(cat {})\" != 'seed' ]; then \
            echo 'FAIL: extra read-only root ReadFile failed' >&2; \
            exit 1; \
         fi\n",
        ro_file.display(),
    ));

    // Negative: overwriting an EXISTING file in the read-only root must be
    // denied. This is the unambiguous WriteFile assertion — the file is already
    // there, so denial cannot be attributed to a missing `MakeReg`, which is
    // what a create-only probe would actually be testing.
    script.push_str(&format!(
        "if echo bad > {} 2>/dev/null; then \
            echo 'FAIL: extra read-only root should have denied WriteFile on an existing file' >&2; \
            exit 1; \
         fi\n",
        ro_file.display(),
    ));

    // Negative: creating a new file in the read-only root must also be denied.
    script.push_str(&format!(
        "if echo bad > {} 2>/dev/null; then \
            echo 'FAIL: extra read-only root should have denied file creation' >&2; \
            exit 1; \
         fi\n",
        ro_write_target.display(),
    ));

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
        "extra allowed-roots contract failed: rw root must be read/write, \
         ro root must be read-only; exit status: {status}",
    );

    assert!(
        !ro_write_target.exists(),
        "sandboxed child must NOT have been able to create a file in the read-only extra root",
    );
    assert_eq!(
        std::fs::read_to_string(&ro_file).expect("parent must read the read-only root's file"),
        "seed",
        "sandboxed child must NOT have been able to overwrite an existing file \
         in the read-only extra root",
    );

    let _ = std::fs::remove_dir_all(&extra_rw);
    let _ = std::fs::remove_dir_all(&extra_ro);
}

/// The write-only tier must support actually delivering a file — creating a new
/// one and writing an existing one — while never permitting reads.
///
/// The previous coverage supplied only read-write and read-only roots, so
/// `allowed_roots_write_only` reached the spawned-process boundary untested and
/// its advertised behavior rested on the permission bits alone.
#[test]
fn landlock_write_only_root_allows_delivery_without_read() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let extra_wo = outside_root("wo");

    let existing = extra_wo.join("existing.txt");
    let created = extra_wo.join("created.txt");
    std::fs::write(&existing, "seed").expect("parent must seed write-only root content");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        Vec::new(),
        vec![extra_wo.clone()],
    )
    .expect("landlock should succeed on linux with feature enabled");

    // Positive: create a new file. This is the common case — delivering an
    // output artifact into a drop directory — and needs `MakeReg` on the parent.
    assert!(
        run_sandboxed(&sandbox, &format!("echo created > {}", created.display())),
        "write-only root must permit creating a new file",
    );
    assert_eq!(
        std::fs::read_to_string(&created).expect("parent must read the created file"),
        "created\n",
        "write-only root must have actually written the new file's content",
    );

    // Positive: overwrite an existing file.
    assert!(
        run_sandboxed(&sandbox, &format!("echo replaced > {}", existing.display())),
        "write-only root must permit writing an existing file",
    );
    assert_eq!(
        std::fs::read_to_string(&existing).expect("parent must read the existing file"),
        "replaced\n",
        "write-only root must have actually replaced the existing file's content",
    );

    // Negative: reading must stay denied, or the tier is not write-only.
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("cat {} > /dev/null 2>/dev/null", existing.display()),
        ),
        "write-only root must deny ReadFile",
    );

    let _ = std::fs::remove_dir_all(&extra_wo);
}

/// Regression: an absent extra root must not disable sandboxing wholesale.
///
/// The extra roots are policy-generated, not operator-typed:
/// `SecurityPolicy::for_agent` unconditionally adds `<install>/shared/skills` to
/// the read-only tier, and fresh config initialization creates `<install>/shared`
/// without that child. When a missing root was fatal, `build_ruleset` failed
/// inside `wrap_command`, so the first absent path stopped *every* sandboxed
/// command from spawning — while detection and posture still reported Landlock
/// as active, because they only ran a minimal kernel probe.
#[test]
fn landlock_absent_extra_root_does_not_disable_sandbox() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let absent = workspace.path().join("generated-but-not-yet-created");
    assert!(!absent.exists(), "the absent root must not exist");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![absent],
        Vec::new(),
    )
    .expect("an absent policy-generated root must not make Landlock unavailable");

    // The sandbox must still spawn commands at all.
    assert!(
        run_sandboxed(&sandbox, "echo hello > /dev/null"),
        "a sandboxed command must still spawn when an extra root is absent",
    );

    // Skipping the absent root must not loosen anything else: the workspace
    // boundary still has to hold.
    let outside = outside_root("absent_boundary");
    let denied = outside.join("should_not_be_created.txt");
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("echo bad > {} 2>/dev/null", denied.display())
        ),
        "skipping an absent root must not grant access outside the workspace",
    );
    assert!(!denied.exists(), "the outside write must not have happened");

    let _ = std::fs::remove_dir_all(&outside);
}

/// Create a unique, empty directory directly beneath `/tmp`.
///
/// The opposite of [`outside_root`]: this one is deliberately *inside* the
/// generic allow-list, which already grants `/tmp`
/// `ReadFile | WriteFile | Truncate` recursively.
fn tmp_root(tag: &str) -> PathBuf {
    let dir = Path::new("/tmp").join(format!("zc_overlap_{tag}_{}", std::process::id()));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("failed to clear leftover /tmp root");
    }
    std::fs::create_dir_all(&dir).expect("failed to create /tmp root");
    dir
}

/// Regression: a restrictive tier root that overlaps a broader rule must not
/// have its rule installed, because Landlock cannot honour it.
///
/// Landlock rules are additive within a layer — for a given file the kernel
/// unions the rights of every rule whose subtree contains it — so a rule can
/// never subtract a right a broader rule already granted. `/tmp` is granted
/// `ReadFile | WriteFile | Truncate` recursively, so a read-only root beneath
/// `/tmp` stays writable and a write-only root beneath it stays readable no
/// matter what their own rules say. Installing those rules anyway advertised a
/// boundary the kernel does not implement.
///
/// The skip is observable because the tiers grant rights `/tmp` does not:
/// `ReadDir` for the read-only tier and `MakeReg` for the write-only tier. If
/// the rules were installed, listing the read-only root and creating a file in
/// the write-only root would both succeed. The matching roots outside `/tmp`
/// prove the denials come from the skip and not from the environment.
#[test]
fn landlock_tier_root_beneath_generic_rule_is_not_installed() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let overlapping_ro = tmp_root("ro");
    let overlapping_wo = tmp_root("wo");
    let enforceable_ro = outside_root("enforceable_ro");
    let enforceable_wo = outside_root("enforceable_wo");

    std::fs::write(overlapping_ro.join("seed.txt"), "seed").expect("parent must seed the ro root");
    std::fs::write(enforceable_ro.join("seed.txt"), "seed").expect("parent must seed the ro root");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![overlapping_ro.clone(), enforceable_ro.clone()],
        vec![overlapping_wo.clone(), enforceable_wo.clone()],
    )
    .expect("an unenforceable root must not make Landlock unavailable");

    // The sandbox must stay usable: skipping is not an outage.
    assert!(
        run_sandboxed(&sandbox, "echo hello > /dev/null"),
        "a sandboxed command must still spawn when a tier root is unenforceable",
    );

    // Read-only tier: `ReadDir` is granted by the tier and not by `/tmp`.
    assert!(
        run_sandboxed(
            &sandbox,
            &format!("ls {} > /dev/null 2>&1", enforceable_ro.display())
        ),
        "an enforceable read-only root must be listable, or the denial below proves nothing",
    );
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("ls {} > /dev/null 2>&1", overlapping_ro.display())
        ),
        "the read-only rule for a root beneath /tmp must not have been installed",
    );

    // Write-only tier: `MakeReg` is granted by the tier and not by `/tmp`.
    let enforceable_new = enforceable_wo.join("created.txt");
    let overlapping_new = overlapping_wo.join("created.txt");
    assert!(
        run_sandboxed(
            &sandbox,
            &format!("echo created > {}", enforceable_new.display())
        ),
        "an enforceable write-only root must permit creation, or the denial below proves nothing",
    );
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("echo created > {} 2>/dev/null", overlapping_new.display())
        ),
        "the write-only rule for a root beneath /tmp must not have been installed",
    );
    assert!(
        !overlapping_new.exists(),
        "the write-only creation beneath /tmp must not have happened",
    );

    // Skipping withholds a rule; it does not withdraw access. The path keeps
    // exactly the rights `/tmp` already gave it, which is what it had before any
    // tier was propagated — the point is that the tier is no longer advertised
    // as a boundary it never was. The WARN emitted at construction is the
    // operator-facing half of this contract.
    assert!(
        run_sandboxed(
            &sandbox,
            &format!(
                "cat {} > /dev/null",
                overlapping_ro.join("seed.txt").display()
            )
        ),
        "skipping the rule must leave /tmp's pre-existing ReadFile grant untouched",
    );

    for dir in [
        &overlapping_ro,
        &overlapping_wo,
        &enforceable_ro,
        &enforceable_wo,
    ] {
        let _ = std::fs::remove_dir_all(dir);
    }
}

/// A read-only root *inside* the workspace keeps the workspace's write right.
///
/// This is composition, not a bypass: `SecurityPolicy` grants read and write
/// everywhere beneath `workspace_dir` before it ever consults the tiers
/// (`is_resolved_path_allowed` returns at the workspace check), so read+write is
/// the application-layer contract for that path too. Landlock unions the
/// workspace rule with the tier rule and lands on the same answer. The tier rule
/// is installed — it simply adds nothing the workspace had not already granted.
#[test]
fn landlock_tier_root_inside_workspace_composes_with_workspace_grant() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let nested_ro = workspace.path().join("declared-read-only");
    std::fs::create_dir_all(&nested_ro).expect("failed to create nested root");
    let nested_file = nested_ro.join("seed.txt");
    std::fs::write(&nested_file, "seed").expect("parent must seed the nested root");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![nested_ro.clone()],
        Vec::new(),
    )
    .expect("an unenforceable root must not make Landlock unavailable");

    assert!(
        run_sandboxed(
            &sandbox,
            &format!("echo overwritten > {}", nested_file.display())
        ),
        "a read-only root inside the workspace composes with the workspace's \
         read-write grant, exactly as SecurityPolicy composes them",
    );
    assert_eq!(
        std::fs::read_to_string(&nested_file).expect("parent must read the nested file"),
        "overwritten\n",
    );

    // Skipping must not disturb the boundary that *is* enforceable.
    let outside = outside_root("workspace_overlap_boundary");
    let denied = outside.join("should_not_be_created.txt");
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("echo bad > {} 2>/dev/null", denied.display())
        ),
        "the nested tier root must not grant access outside the workspace",
    );
    assert!(!denied.exists(), "the outside write must not have happened");

    let _ = std::fs::remove_dir_all(&outside);
}

/// Regression: nested restrictive tiers must compose, not cancel.
///
/// `SecurityPolicy` resolves a path by taking the first tier that matches:
/// `is_resolved_path_readable` admits anything beneath a read-only root, and
/// `is_resolved_path_allowed` admits anything beneath a write-only root. A
/// write-only root nested inside a read-only root is therefore both readable
/// and writable at the application layer, and Landlock — which unions the
/// rights of every rule covering a path — reproduces that by installing both
/// rules.
///
/// An earlier revision of the overlap check compared each tier against every
/// other grant, so the read-only parent saw the write-only child's write rights
/// and the write-only child saw the read-only parent's read rights, and *both*
/// rules were skipped. That left a valid configuration with neither read nor
/// write at the child boundary.
#[test]
fn landlock_read_only_parent_with_write_only_child_composes() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let parent = outside_root("compose_ro_parent");
    let child = parent.join("write-only-child");
    std::fs::create_dir_all(&child).expect("failed to create nested write-only root");

    let parent_file = parent.join("parent.txt");
    let child_file = child.join("child.txt");
    std::fs::write(&parent_file, "seed").expect("parent must seed the read-only root");
    std::fs::write(&child_file, "seed").expect("parent must seed the nested root");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![parent.clone()],
        vec![child.clone()],
    )
    .expect("landlock should succeed on linux with feature enabled");

    // Read comes from the read-only parent, recursively.
    assert!(
        run_sandboxed(
            &sandbox,
            &format!("cat {} > /dev/null", child_file.display())
        ),
        "the nested write-only root must stay readable through the read-only parent",
    );
    // Write comes from the write-only child: `MakeReg` is granted by that tier
    // and by neither the parent nor any generic rule.
    let created = child.join("created.txt");
    assert!(
        run_sandboxed(&sandbox, &format!("echo created > {}", created.display())),
        "the nested write-only root must permit creating a file",
    );
    assert_eq!(
        std::fs::read_to_string(&created).expect("parent must read the created file"),
        "created\n",
    );

    // The read-only parent keeps its own boundary outside the nested child.
    assert!(
        run_sandboxed(
            &sandbox,
            &format!("cat {} > /dev/null", parent_file.display())
        ),
        "the read-only parent must stay readable",
    );
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("echo bad > {} 2>/dev/null", parent_file.display())
        ),
        "the read-only parent must stay read-only outside the nested write-only child",
    );
    assert_eq!(
        std::fs::read_to_string(&parent_file).expect("parent must read its file"),
        "seed",
    );

    let _ = std::fs::remove_dir_all(&parent);
}

/// The mirror image: a read-only root nested inside a write-only root.
///
/// The composed contract is the same — read from the nested read-only rule,
/// write from the enclosing write-only rule — and the enclosing root stays
/// unreadable outside the nested child.
#[test]
fn landlock_write_only_parent_with_read_only_child_composes() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let parent = outside_root("compose_wo_parent");
    let child = parent.join("read-only-child");
    std::fs::create_dir_all(&child).expect("failed to create nested read-only root");

    let parent_file = parent.join("parent.txt");
    let child_file = child.join("child.txt");
    std::fs::write(&parent_file, "seed").expect("parent must seed the write-only root");
    std::fs::write(&child_file, "seed").expect("parent must seed the nested root");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![child.clone()],
        vec![parent.clone()],
    )
    .expect("landlock should succeed on linux with feature enabled");

    assert!(
        run_sandboxed(
            &sandbox,
            &format!("cat {} > /dev/null", child_file.display())
        ),
        "the nested read-only root must be readable",
    );
    let created = child.join("created.txt");
    assert!(
        run_sandboxed(&sandbox, &format!("echo created > {}", created.display())),
        "the nested root must stay writable through the enclosing write-only parent",
    );
    assert_eq!(
        std::fs::read_to_string(&created).expect("parent must read the created file"),
        "created\n",
    );

    // The write-only parent stays unreadable where the nested read grant does
    // not reach.
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("cat {} > /dev/null 2>/dev/null", parent_file.display())
        ),
        "the write-only parent must stay unreadable outside the nested read-only child",
    );

    let _ = std::fs::remove_dir_all(&parent);
}

/// The degenerate overlap: one path listed in both restrictive tiers.
///
/// `SecurityPolicy` admits it for reads via the read-only list and for writes
/// via the write-only list, so the composed contract is read+write and the two
/// rules must union rather than cancel.
#[test]
fn landlock_root_in_both_restrictive_tiers_composes() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");
    let root = outside_root("compose_both_tiers");
    let seeded = root.join("seed.txt");
    std::fs::write(&seeded, "seed").expect("parent must seed the root");

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![root.clone()],
        vec![root.clone()],
    )
    .expect("landlock should succeed on linux with feature enabled");

    assert!(
        run_sandboxed(&sandbox, &format!("cat {} > /dev/null", seeded.display())),
        "a root in both restrictive tiers must be readable",
    );
    let created = root.join("created.txt");
    assert!(
        run_sandboxed(&sandbox, &format!("echo created > {}", created.display())),
        "a root in both restrictive tiers must be writable",
    );
    assert_eq!(
        std::fs::read_to_string(&created).expect("parent must read the created file"),
        "created\n",
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// Regression: an extra root that cannot be opened for *any* reason must not
/// disable the whole sandbox.
///
/// `landlock_absent_extra_root_does_not_disable_sandbox` covers `ENOENT`. This
/// covers everything else: a path beneath a non-directory (`/dev/null/not-a-child`)
/// fails `PathFd::new` with `ENOTDIR`, which an earlier revision propagated out
/// of `build_ruleset`. `create_selected_sandbox` discards that error, so the
/// factory returned `NoopSandbox` — one malformed optional root stripped kernel
/// enforcement from the valid workspace and every other valid root.
///
/// The assertion is therefore not merely that construction succeeds, but that
/// the child is still confined: it can write inside the workspace and cannot
/// write outside it.
#[test]
fn landlock_unopenable_extra_root_does_not_disable_sandbox() {
    let _serialized = serialize_test();
    use tempfile::tempdir;

    let workspace = tempdir().expect("failed to create temp directory");

    // Beneath a non-directory: open(2) fails with ENOTDIR, not ENOENT.
    let invalid = PathBuf::from("/dev/null/not-a-child");
    let dev_null = Path::new("/dev/null");
    assert!(
        dev_null.exists() && !dev_null.is_dir(),
        "the probe relies on /dev/null existing as a non-directory",
    );

    let sandbox = LandlockSandbox::with_roots(
        Some(workspace.path().to_path_buf()),
        Vec::new(),
        vec![invalid.clone()],
        vec![invalid],
    )
    .expect("an unopenable extra root must not make Landlock unavailable");

    // The workspace rule must still be installed and enforced.
    let inside = workspace.path().join("inside.txt");
    assert!(
        run_sandboxed(&sandbox, &format!("echo ok > {}", inside.display())),
        "the workspace must still be writable when an extra root could not be opened",
    );
    assert_eq!(
        std::fs::read_to_string(&inside).expect("parent must read the workspace file"),
        "ok\n",
    );

    // And the child must still be confined — this is what fails if the sandbox
    // silently degraded to a no-op.
    let outside = outside_root("unopenable_boundary");
    let denied = outside.join("should_not_be_created.txt");
    assert!(
        !run_sandboxed(
            &sandbox,
            &format!("echo bad > {} 2>/dev/null", denied.display())
        ),
        "an unopenable extra root must not drop the child out of the sandbox",
    );
    assert!(!denied.exists(), "the outside write must not have happened");

    let _ = std::fs::remove_dir_all(&outside);
}
