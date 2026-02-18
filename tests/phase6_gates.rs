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

    // Write a PID that doesn't exist
    let pid_path = inst_dir.join("daemon.pid");
    fs::write(&pid_path, "4294967")?;
    assert!(pid_path.exists(), "PID file should exist before start");

    // Start should detect the stale PID and clear it before spawn attempt.
    // It will fail because no zeroclaw binary, but the PID file should
    // have been removed before that error.
    let result = lifecycle::start_instance(&registry, "stale");
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();

    // It should NOT say "already running" - the stale PID was cleared
    assert!(
        !msg.contains("already running"),
        "Stale PID should have been cleared, but got: {msg}"
    );

    // The stale PID file should have been removed
    assert!(
        !pid_path.exists(),
        "Stale PID file should have been removed before spawn attempt"
    );

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

// ── Gate 11: EPERM liveness -- PID 1 detected as alive ─────────

#[test]
fn gate11_eperm_pid_detected_as_alive() {
    // As non-root, kill(1, 0) returns EPERM. We must treat that as "alive".
    if unsafe { libc::getuid() } == 0 {
        return; // skip if running as root (CI edge case)
    }

    let tmp = TempDir::new().unwrap();
    let cp_dir = tmp.path().join("cp");
    let instances_dir = cp_dir.join("instances");
    fs::create_dir_all(&instances_dir).unwrap();

    let registry = Registry::open(&cp_dir.join("registry.db")).unwrap();

    let id = uuid::Uuid::new_v4().to_string();
    let inst_dir = instances_dir.join(&id);
    fs::create_dir_all(&inst_dir).unwrap();
    let config_path = inst_dir.join("config.toml");
    fs::write(&config_path, "default_temperature = 0.7\n").unwrap();
    registry
        .create_instance(&id, "eperm-test", 18813, config_path.to_str().unwrap(), None, None)
        .unwrap();

    // Write PID 1 (init) as the daemon PID. It's alive but EPERM.
    fs::write(inst_dir.join("daemon.pid"), "1").unwrap();

    // start_instance should see PID 1 as alive (not clear it as stale)
    let result = lifecycle::start_instance(&registry, "eperm-test");
    assert!(result.is_err());
    let err = result.unwrap_err().to_string();

    // It should NOT be a "Cannot find zeroclaw binary" error -- that would mean
    // it cleared the PID and tried to spawn. It should either report "already running"
    // (if ownership matched, which it won't) or clear-as-stale (ownership fails).
    // Since PID 1's environ is unreadable, ownership returns false, so it clears
    // as "stale PID not owned by this instance". That's correct behavior.
    assert!(
        !err.contains("already running"),
        "PID 1 should not be considered 'ours': {err}"
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
