use anyhow::Result;
use std::fs;
use std::os::unix::io::AsRawFd;
use tempfile::TempDir;
use zeroclaw::db::Registry;
use zeroclaw::lifecycle;

/// Helper: create a registry with a registered instance in a temp dir.
/// Returns (TempDir, Registry, instance_id, instance_dir).
fn setup_instance(name: &str, port: u16) -> (TempDir, Registry, String, std::path::PathBuf) {
    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    let registry = Registry::open(&cp_dir.join("registry.db")).unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir).unwrap();

    // Create a minimal config.toml so the instance looks valid
    let config_path = inst_dir.join("config.toml");
    fs::write(
        &config_path,
        format!(
            r#"default_temperature = 0.7

[gateway]
port = {port}
host = "127.0.0.1"
require_pairing = true
"#
        ),
    )
    .unwrap();

    registry
        .create_instance(
            &id,
            name,
            port,
            config_path.to_str().unwrap(),
            None,
            None,
        )
        .unwrap();

    (tmp, registry, id, inst_dir)
}

// ── Gate 1: Registry update_status works ────────────────────────

#[test]
fn gate1_registry_update_status() -> Result<()> {
    let (_tmp, registry, id, _inst_dir) = setup_instance("test-agent", 18801);

    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(inst.status, "stopped");

    registry.update_status(&id, "running")?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(inst.status, "running");

    registry.update_status(&id, "stopped")?;
    let inst = registry.get_instance(&id)?.unwrap();
    assert_eq!(inst.status, "stopped");

    Ok(())
}

// ── Gate 2: Status shows stopped for fresh instance ────────────

#[test]
fn gate2_status_shows_stopped_for_fresh_instance() -> Result<()> {
    let (_tmp, registry, _id, _inst_dir) = setup_instance("fresh", 18802);

    lifecycle::show_status(&registry, Some("fresh"))?;
    lifecycle::show_status(&registry, None)?;

    Ok(())
}

// ── Gate 3: PID ownership check rejects wrong process ──────────

#[test]
fn gate3_pid_ownership_rejects_wrong_process() -> Result<()> {
    let (_tmp, registry, _id, inst_dir) = setup_instance("owned", 18803);

    // Write our own PID as the daemon PID
    let our_pid = std::process::id();
    fs::write(inst_dir.join("daemon.pid"), our_pid.to_string())?;

    // stop_instance should refuse to kill because we're not the daemon
    let result = lifecycle::stop_instance(&registry, "owned");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("does NOT belong"),
        "Expected ownership rejection, got: {err}"
    );

    // PID file should be preserved (not removed) since we refused to act
    assert!(
        inst_dir.join("daemon.pid").exists(),
        "PID file must be preserved when ownership check fails"
    );

    Ok(())
}

// ── Gate 4: Stale PID cleared on start attempt ─────────────────

#[test]
fn gate4_stale_pid_cleared() -> Result<()> {
    let (_tmp, registry, _id, inst_dir) = setup_instance("stale", 18804);

    // Write a PID that doesn't exist (definitely dead)
    let pid_path = inst_dir.join("daemon.pid");
    let stale_pid: u32 = 4_294_967;
    fs::write(&pid_path, stale_pid.to_string())?;
    assert!(pid_path.exists(), "PID file should exist before start");

    // Start should detect the stale PID and clear it. Depending on whether
    // a zeroclaw binary is discoverable, the spawn may succeed or fail.
    // Either way, the stale PID must NOT be retained.
    let result = lifecycle::start_instance(&registry, "stale");

    // If it errored, it must not be "already running" (that would mean the
    // stale PID was treated as alive, which is wrong for a dead PID).
    if let Err(ref e) = result {
        assert!(
            !e.to_string().contains("already running"),
            "Dead PID {stale_pid} must not be treated as alive: {e}"
        );
        // Spawn failed after clearing stale PID -- pidfile should be gone
        assert!(
            !pid_path.exists(),
            "Stale PID file should have been removed before spawn attempt"
        );
    }

    // If spawn succeeded, the pidfile should contain a NEW pid (not the stale one)
    if result.is_ok() {
        assert!(pid_path.exists(), "PID file should exist after successful start");
        let current_pid: u32 = fs::read_to_string(&pid_path)?.trim().parse()?;
        assert_ne!(
            current_pid, stale_pid,
            "Stale PID must have been replaced, not retained"
        );
        // Clean up: stop the accidentally-started instance
        let _ = lifecycle::stop_instance(&registry, "stale");
        if pid_path.exists() {
            // Fallback: direct kill if stop failed (e.g., ownership check on fake binary)
            unsafe { libc::kill(current_pid as libc::pid_t, libc::SIGKILL); }
            let _ = fs::remove_file(&pid_path);
        }
    }

    Ok(())
}

// ── Gate 5: Lifecycle lock prevents concurrent operations ──────

#[test]
fn gate5_lifecycle_lock_prevents_concurrent_ops() -> Result<()> {
    let (_tmp, registry, _id, inst_dir) = setup_instance("locked", 18805);

    // Acquire the lifecycle lock manually (simulating a concurrent start/stop)
    let lock_path = inst_dir.join("lifecycle.lock");
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)?;
    let ret = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    assert_eq!(ret, 0, "Should acquire lock");

    // start should fail with lock error
    let result = lifecycle::start_instance(&registry, "locked");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Lifecycle lock held"),
        "Expected lock rejection, got: {err}"
    );

    // stop should fail with lock error
    let result = lifecycle::stop_instance(&registry, "locked");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Lifecycle lock held"),
        "Expected lock rejection for stop, got: {err}"
    );

    // restart should fail with lock error
    let result = lifecycle::restart_instance(&registry, "locked");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("Lifecycle lock held"),
        "Expected lock rejection for restart, got: {err}"
    );

    Ok(())
}

// ── Gate 6: Log rotation on start ──────────────────────────────

#[test]
fn gate6_log_rotation() -> Result<()> {
    let (_tmp, _registry, _id, inst_dir) = setup_instance("rotated", 18806);

    // Create a pre-existing log file
    let logs_dir = inst_dir.join("logs");
    fs::create_dir_all(&logs_dir)?;
    let log_file = logs_dir.join("daemon.log");
    fs::write(&log_file, "old log content\n")?;

    // Start will rotate the log before attempting to spawn (which will fail)
    let _ = lifecycle::start_instance(&_registry, "rotated");

    // The old log should have been rotated to .log.1
    let rotated = logs_dir.join("daemon.log.1");
    assert!(rotated.exists(), "Rotated log should exist");
    assert_eq!(
        fs::read_to_string(&rotated)?,
        "old log content\n",
        "Rotated log should have old content"
    );

    Ok(())
}

// ── Gate 7: show_logs reads log content ────────────────────────

#[test]
fn gate7_show_logs_reads_content() -> Result<()> {
    let (_tmp, _registry, _id, inst_dir) = setup_instance("logged", 18807);

    // No log file yet - should error
    let result = lifecycle::show_logs(&inst_dir, 10, false);
    assert!(result.is_err());

    // Create log content
    let logs_dir = inst_dir.join("logs");
    fs::create_dir_all(&logs_dir)?;
    let content: String = (1..=20).map(|i| format!("log line {i}\n")).collect();
    fs::write(logs_dir.join("daemon.log"), content)?;

    // Should read without error
    lifecycle::show_logs(&inst_dir, 5, false)?;

    Ok(())
}

// ── Gate 8: Stop on non-running instance gives clear error ─────

#[test]
fn gate8_stop_non_running_gives_clear_error() -> Result<()> {
    let (_tmp, registry, _id, _inst_dir) = setup_instance("idle", 18808);

    let result = lifecycle::stop_instance(&registry, "idle");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not running"),
        "Expected 'not running' error, got: {err}"
    );

    Ok(())
}

// ── Gate 9: Start/stop on non-existent instance gives clear error

#[test]
fn gate9_nonexistent_instance_error() -> Result<()> {
    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    fs::create_dir_all(&cp_dir)?;
    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    let result = lifecycle::start_instance(&registry, "ghost");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No instance named"));

    let result = lifecycle::stop_instance(&registry, "ghost");
    assert!(result.is_err());
    assert!(result.unwrap_err().to_string().contains("No instance named"));

    Ok(())
}

// ── Gate 10: Fleet status with multiple instances ──────────────

#[test]
fn gate10_fleet_status_multiple_instances() -> Result<()> {
    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir)?;

    let registry = Registry::open(&cp_dir.join("registry.db"))?;

    for (name, port) in &[("alpha", 18810), ("beta", 18811), ("gamma", 18812)] {
        let id = uuid::Uuid::new_v4().to_string();
        let inst_dir = instances_dir.join(&id);
        fs::create_dir_all(&inst_dir)?;
        let config_path = inst_dir.join("config.toml");
        fs::write(&config_path, "default_temperature = 0.7\n")?;
        registry.create_instance(
            &id,
            name,
            *port,
            config_path.to_str().unwrap(),
            None,
            None,
        )?;
    }

    lifecycle::show_status(&registry, None)?;
    lifecycle::show_status(&registry, Some("alpha"))?;
    lifecycle::show_status(&registry, Some("beta"))?;

    Ok(())
}

// ── Gate 11: EPERM liveness -- direct assertion on is_pid_alive ─

#[test]
fn gate11_eperm_pid_detected_as_alive() {
    // As non-root, kill(1, 0) returns -1 with errno=EPERM.
    // is_pid_alive must return true (process exists, just can't signal it).
    if unsafe { libc::getuid() } == 0 {
        return; // skip if root -- kill(1,0) returns 0 directly
    }

    // Direct liveness assertion: PID 1 is alive via EPERM
    assert!(
        lifecycle::is_pid_alive(1),
        "PID 1 must be detected as alive (EPERM means exists, not dead)"
    );

    // Counter-assertion: impossible PID is dead (ESRCH)
    assert!(
        !lifecycle::is_pid_alive(4_294_967),
        "Impossible PID must be detected as dead (ESRCH)"
    );

    // Our own PID is alive (returns 0 directly)
    assert!(
        lifecycle::is_pid_alive(std::process::id()),
        "Own PID must be detected as alive"
    );
}

// ── Gate 13: Rollback kills child on post-spawn bookkeeping failure

#[test]
fn gate13_rollback_kills_child_on_write_pid_failure() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = TempDir::new().unwrap();
    let inst_dir = tmp.path().join("instance");
    fs::create_dir_all(&inst_dir).unwrap();

    // Pre-create logs dir (writable) and lifecycle.lock (writable file)
    // so they work even after inst_dir becomes read-only.
    let logs_dir = inst_dir.join("logs");
    fs::create_dir_all(&logs_dir).unwrap();
    let lock_file = inst_dir.join("lifecycle.lock");
    fs::write(&lock_file, "").unwrap();

    // Create a fake zeroclaw binary that just sleeps
    let fake_bin = tmp.path().join("fake-zeroclaw");
    fs::write(&fake_bin, "#!/bin/sh\nexec sleep 3600\n").unwrap();
    fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o755)).unwrap();

    // Register instance
    let registry = Registry::open(&tmp.path().join("registry.db")).unwrap();
    let config_path = inst_dir.join("config.toml");
    fs::write(&config_path, "default_temperature = 0.7\n").unwrap();
    registry
        .create_instance(
            "rollback-id",
            "rollback-test",
            18900,
            config_path.to_str().unwrap(),
            None,
            None,
        )
        .unwrap();

    // Make instance dir read-only: lifecycle.lock can still be opened (existing file),
    // logs/ subdir is still writable, but daemon.pid creation will fail (EACCES).
    fs::set_permissions(&inst_dir, fs::Permissions::from_mode(0o555)).unwrap();

    std::env::set_var("ZEROCLAW_BIN", fake_bin.to_str().unwrap());
    let result = lifecycle::start_instance(&registry, "rollback-test");
    std::env::remove_var("ZEROCLAW_BIN");

    // Restore permissions so TempDir cleanup works
    fs::set_permissions(&inst_dir, fs::Permissions::from_mode(0o755)).unwrap();

    // 1. Should have failed with PID write error + rollback message
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("killed spawned daemon"),
        "Error should mention rollback kill, got: {err}"
    );

    // 2. DB status should still be stopped (never updated)
    let inst = registry.get_instance("rollback-id").unwrap().unwrap();
    assert_eq!(
        inst.status, "stopped",
        "DB must not be updated to 'running' after rollback"
    );

    // 3. No PID file should exist
    assert!(
        !inst_dir.join("daemon.pid").exists(),
        "PID file must not exist after rollback"
    );

    // 4. Verify child was killed: scan /proc for any process with our ZEROCLAW_HOME
    std::thread::sleep(std::time::Duration::from_millis(200));
    let needle = format!("ZEROCLAW_HOME={}", inst_dir.to_string_lossy());
    let mut orphan_found = false;
    if let Ok(entries) = fs::read_dir("/proc") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if !name.chars().all(|c| c.is_ascii_digit()) {
                continue;
            }
            let environ_path = format!("/proc/{name}/environ");
            if let Ok(data) = fs::read(&environ_path) {
                for chunk in data.split(|&b| b == 0) {
                    if let Ok(s) = std::str::from_utf8(chunk) {
                        if s == needle {
                            orphan_found = true;
                            // Kill it so we don't leak even if assertion fails
                            if let Ok(pid) = name.parse::<i32>() {
                                unsafe { libc::kill(pid, libc::SIGKILL); }
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(
        !orphan_found,
        "Orphaned child process must be killed during rollback"
    );
}

// ── Gate 12: Runtime start/status/stop (requires binary) ───────

#[test]
#[ignore]
fn gate12_runtime_start_status_stop() {
    let zeroclaw_bin = std::env::var("ZEROCLAW_BIN")
        .unwrap_or_else(|_| panic!("ZEROCLAW_BIN required for runtime lifecycle gate"));

    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    let registry = Registry::open(&cp_dir.join("registry.db")).unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir).unwrap();

    let config_path = inst_dir.join("config.toml");
    fs::write(
        &config_path,
        r#"default_temperature = 0.7

[gateway]
port = 18899
host = "127.0.0.1"
require_pairing = true
"#,
    )
    .unwrap();

    // Create workspace dir (daemon expects it)
    fs::create_dir_all(inst_dir.join("workspace")).unwrap();

    registry
        .create_instance(
            &id,
            "runtime-test",
            18899,
            config_path.to_str().unwrap(),
            None,
            None,
        )
        .unwrap();

    std::env::set_var("ZEROCLAW_BIN", &zeroclaw_bin);

    // Start
    lifecycle::start_instance(&registry, "runtime-test").unwrap();

    // Verify PID file exists
    let pid_path = inst_dir.join("daemon.pid");
    assert!(pid_path.exists(), "PID file should exist after start");
    let pid: u32 = fs::read_to_string(&pid_path)
        .unwrap()
        .trim()
        .parse()
        .unwrap();

    // Verify process is alive
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    assert!(alive, "Daemon process should be alive");

    // Verify DB status
    let inst = registry.get_instance(&id).unwrap().unwrap();
    assert_eq!(inst.status, "running");

    // Status should show running
    lifecycle::show_status(&registry, Some("runtime-test")).unwrap();

    // Wait a moment for daemon to initialize
    std::thread::sleep(std::time::Duration::from_secs(2));

    // Stop
    lifecycle::stop_instance(&registry, "runtime-test").unwrap();

    // Verify PID file removed
    assert!(!pid_path.exists(), "PID file should be removed after stop");

    // Verify process is dead
    let alive = unsafe { libc::kill(pid as libc::pid_t, 0) == 0 };
    assert!(!alive, "Daemon process should be dead after stop");

    // Verify DB status
    let inst = registry.get_instance(&id).unwrap().unwrap();
    assert_eq!(inst.status, "stopped");

    std::env::remove_var("ZEROCLAW_BIN");
}
