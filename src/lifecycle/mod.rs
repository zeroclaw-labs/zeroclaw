use anyhow::{bail, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::db::{Instance, Registry};

// ── Typed lifecycle errors ─────────────────────────────────────

/// Typed error for lifecycle operations, enabling HTTP handlers to
/// pattern-match on variants and return correct status codes.
#[derive(Debug)]
pub enum LifecycleError {
    /// Instance not found in registry.
    NotFound(String),
    /// Instance is already running.
    AlreadyRunning(String),
    /// Instance is not running (cannot stop).
    NotRunning(String),
    /// Lifecycle lock is held by another operation.
    LockHeld,
    /// Any other error.
    Internal(anyhow::Error),
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(name) => write!(f, "No instance named '{name}'"),
            Self::AlreadyRunning(name) => write!(f, "Instance '{name}' is already running"),
            Self::NotRunning(name) => write!(f, "Instance '{name}' is not running"),
            Self::LockHeld => write!(f, "Lifecycle lock held (concurrent operation in progress)"),
            Self::Internal(e) => write!(f, "{e:#}"),
        }
    }
}

impl std::error::Error for LifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Internal(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

impl From<anyhow::Error> for LifecycleError {
    fn from(e: anyhow::Error) -> Self {
        Self::Internal(e)
    }
}

/// PID file name within each instance directory.
const PID_FILE: &str = "daemon.pid";

/// Lock file name for lifecycle operations.
const LIFECYCLE_LOCK: &str = "lifecycle.lock";

/// Log directory name within each instance directory.
const LOG_DIR: &str = "logs";

/// Log file name within the log directory.
const LOG_FILE: &str = "daemon.log";

/// Rotated log file name.
const LOG_FILE_ROTATED: &str = "daemon.log.1";

/// Default number of log lines to show.
pub const DEFAULT_LOG_LINES: usize = 50;

/// Timeout in seconds waiting for graceful shutdown.
const SHUTDOWN_TIMEOUT_SECS: u64 = 10;

/// Brief delay after spawn to detect immediate crashes.
const POST_SPAWN_CHECK_MS: u64 = 300;

// ── Lifecycle lock ──────────────────────────────────────────────

/// Result type for lifecycle lock acquisition that distinguishes
/// contention from real IO errors.
pub enum LockOutcome {
    /// Lock acquired successfully.
    Acquired(fs::File),
    /// Lock is held by another process (EWOULDBLOCK).
    Contended,
}

/// Acquire per-instance lifecycle lock (non-blocking flock).
/// Returns `LockOutcome::Acquired(file)` on success, `LockOutcome::Contended`
/// if another process holds it, or `Err` for real IO/permission errors.
pub fn try_lifecycle_lock(instance_dir: &Path) -> Result<LockOutcome> {
    let lock_path = instance_dir.join(LIFECYCLE_LOCK);
    let lock = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .open(&lock_path)
        .with_context(|| format!("Failed to open lifecycle lock: {}", lock_path.display()))?;

    let ret = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if ret != 0 {
        let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        if errno == libc::EWOULDBLOCK {
            return Ok(LockOutcome::Contended);
        }
        // Real error (permission, invalid fd, etc.)
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("flock failed on {}", lock_path.display()));
    }

    Ok(LockOutcome::Acquired(lock))
}

/// Convenience wrapper: acquire lock or bail with clear error.
/// Used by lifecycle operations that must hold the lock to proceed.
pub fn acquire_lifecycle_lock(instance_dir: &Path) -> Result<fs::File> {
    match try_lifecycle_lock(instance_dir)? {
        LockOutcome::Acquired(f) => Ok(f),
        LockOutcome::Contended => {
            bail!("Lifecycle lock held (concurrent start/stop in progress?)")
        }
    }
}

// ── Instance directory resolution ───────────────────────────────

/// Resolve the instance directory from registry data.
/// The config_path is `<instance_dir>/config.toml`, so parent is the instance dir.
pub fn instance_dir_from(instance: &Instance) -> PathBuf {
    PathBuf::from(&instance.config_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
}

// ── PID file management ────────────────────────────────────────

/// Read PID from the instance's pidfile. Returns None if file doesn't exist.
pub fn read_pid(instance_dir: &Path) -> Result<Option<u32>> {
    let pid_path = instance_dir.join(PID_FILE);
    match fs::read_to_string(&pid_path) {
        Ok(content) => {
            let pid: u32 = content
                .trim()
                .parse()
                .with_context(|| format!("Invalid PID in {}", pid_path.display()))?;
            Ok(Some(pid))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("Failed to read {}", pid_path.display())),
    }
}

/// Write PID to the instance's pidfile.
pub fn write_pid(instance_dir: &Path, pid: u32) -> Result<()> {
    let pid_path = instance_dir.join(PID_FILE);
    fs::write(&pid_path, pid.to_string())
        .with_context(|| format!("Failed to write PID file: {}", pid_path.display()))
}

/// Remove the PID file. No error if already absent.
pub fn remove_pid(instance_dir: &Path) -> Result<()> {
    let pid_path = instance_dir.join(PID_FILE);
    match fs::remove_file(&pid_path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to remove {}", pid_path.display())),
    }
}

// ── Process checks ─────────────────────────────────────────────

/// Check if a process with the given PID exists.
///
/// Uses `kill(pid, 0)`. Returns true if the process exists, even if we lack
/// permission to signal it (EPERM means "process exists, but you can't signal it").
pub fn is_pid_alive(pid: u32) -> bool {
    let ret = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if ret == 0 {
        return true;
    }
    // errno == EPERM means process exists but we lack permission to signal it.
    // errno == ESRCH means no such process.
    let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
    errno == libc::EPERM
}

/// Verify PID ownership: the process with this PID has `ZEROCLAW_HOME`
/// pointing to the expected instance directory.
///
/// Only trusts `/proc/<pid>/environ` (exact ZEROCLAW_HOME match).
/// If environ is unreadable, returns false (safe default: refuse to act on
/// a process we can't positively identify).
pub fn verify_pid_ownership(pid: u32, expected_instance_dir: &Path) -> Result<bool> {
    let environ_path = format!("/proc/{pid}/environ");
    match fs::read(&environ_path) {
        Ok(data) => {
            let expected = format!(
                "ZEROCLAW_HOME={}",
                expected_instance_dir.to_string_lossy()
            );
            // environ is null-separated key=value pairs
            for entry in data.split(|&b| b == 0) {
                if let Ok(s) = std::str::from_utf8(entry) {
                    if s == expected {
                        return Ok(true);
                    }
                }
            }
            Ok(false)
        }
        Err(_) => {
            // Cannot read environ -- refuse to claim ownership.
            // This is the safe default: we won't kill a process we can't identify.
            Ok(false)
        }
    }
}

// ── Binary resolution ──────────────────────────────────────────

/// Resolve the `zeroclaw` binary path.
/// Priority: `ZEROCLAW_BIN` env var, then sibling of current exe.
fn zeroclaw_bin() -> Result<PathBuf> {
    if let Ok(bin) = std::env::var("ZEROCLAW_BIN") {
        return Ok(PathBuf::from(bin));
    }

    let current =
        std::env::current_exe().context("Failed to resolve current executable path")?;
    let parent = current
        .parent()
        .context("Current executable has no parent directory")?;
    let bin = parent.join("zeroclaw");
    if bin.exists() {
        Ok(bin)
    } else {
        bail!(
            "Cannot find zeroclaw binary at {} (set ZEROCLAW_BIN to override)",
            bin.display()
        );
    }
}

// ── Log management ─────────────────────────────────────────────

/// Log file path for an instance.
pub fn log_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join(LOG_DIR).join(LOG_FILE)
}

/// Rotated log file path.
fn rotated_log_path(instance_dir: &Path) -> PathBuf {
    instance_dir.join(LOG_DIR).join(LOG_FILE_ROTATED)
}

/// Rotate the current log file to `.log.1` before starting.
fn rotate_logs(instance_dir: &Path) -> Result<()> {
    let log = log_path(instance_dir);
    if log.exists() {
        let rotated = rotated_log_path(instance_dir);
        fs::rename(&log, &rotated)
            .with_context(|| format!("Failed to rotate log: {}", log.display()))?;
    }
    Ok(())
}

// ── Internal start/stop (caller must hold lifecycle lock) ──────

/// Kill a child process (best-effort). Used as rollback when post-spawn bookkeeping fails.
fn kill_child_best_effort(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    // Brief wait for cleanup
    std::thread::sleep(std::time::Duration::from_millis(100));
}

/// Stop a running instance. Caller must hold the lifecycle lock.
fn stop_inner(registry: &Registry, instance: &Instance, inst_dir: &Path) -> Result<(), LifecycleError> {
    let pid = read_pid(inst_dir)?
        .ok_or_else(|| LifecycleError::NotRunning(instance.name.clone()))?;

    if !is_pid_alive(pid) {
        tracing::info!("Process {pid} already dead, cleaning up");
        remove_pid(inst_dir)?;
        registry.update_status(&instance.id, "stopped")?;
        println!("Instance '{}' was already stopped (stale PID cleaned)", instance.name);
        return Ok(());
    }

    // Verify ownership before sending any signal
    if !verify_pid_ownership(pid, inst_dir)? {
        return Err(LifecycleError::Internal(anyhow::anyhow!(
            "PID {pid} is alive but does NOT belong to instance '{}'. \
             Refusing to send signal. Remove {} manually if you're sure.",
            instance.name,
            inst_dir.join(PID_FILE).display()
        )));
    }

    // Send SIGTERM
    let ret = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if ret != 0 {
        let err = std::io::Error::last_os_error();
        return Err(LifecycleError::Internal(anyhow::anyhow!(
            "Failed to send SIGTERM to PID {pid}: {err}"
        )));
    }

    // Wait for graceful shutdown
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(SHUTDOWN_TIMEOUT_SECS);
    loop {
        if !is_pid_alive(pid) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            tracing::warn!(
                "PID {pid} did not exit within {SHUTDOWN_TIMEOUT_SECS}s, sending SIGKILL"
            );
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
            // Wait and verify SIGKILL took effect
            std::thread::sleep(std::time::Duration::from_millis(500));
            if is_pid_alive(pid) {
                return Err(LifecycleError::Internal(anyhow::anyhow!(
                    "PID {pid} survived SIGKILL. Process may be in uninterruptible state. \
                     PID file preserved at {}",
                    inst_dir.join(PID_FILE).display()
                )));
            }
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    remove_pid(inst_dir)?;
    registry.update_status(&instance.id, "stopped")?;

    // Best-effort: clear PID cache in DB
    if let Err(e) = registry.update_pid(&instance.id, None) {
        tracing::warn!("Failed to clear PID cache in DB (non-fatal): {e:#}");
    }

    println!("Stopped instance '{}' (was PID {pid})", instance.name);
    Ok(())
}

/// Start an instance by spawning `zeroclaw daemon`. Caller must hold the lifecycle lock.
fn start_inner(registry: &Registry, instance: &Instance, inst_dir: &Path) -> Result<(), LifecycleError> {
    // Check for existing PID
    if let Some(pid) = read_pid(inst_dir)? {
        if is_pid_alive(pid) {
            let owned = verify_pid_ownership(pid, inst_dir)?;
            if owned {
                return Err(LifecycleError::AlreadyRunning(instance.name.clone()));
            }
            tracing::warn!("Stale PID {pid} (not owned by this instance), clearing");
        } else {
            tracing::info!("Clearing stale PID {pid} (process dead)");
        }
        remove_pid(inst_dir)?;
    }

    // Rotate logs
    let logs_dir = inst_dir.join(LOG_DIR);
    fs::create_dir_all(&logs_dir).context("Failed to create log directory")?;
    rotate_logs(inst_dir)?;

    // Spawn daemon
    let bin = zeroclaw_bin()?;
    let log_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path(inst_dir))
        .context("Failed to open log file")?;
    let log_file_err = log_file.try_clone().context("Failed to clone log file handle")?;

    let mut child = Command::new(&bin)
        .arg("daemon")
        .arg("--port")
        .arg(instance.port.to_string())
        .env("ZEROCLAW_HOME", inst_dir.to_string_lossy().as_ref())
        .stdout(log_file)
        .stderr(log_file_err)
        .process_group(0) // Detach from parent's process group
        .spawn()
        .with_context(|| {
            format!(
                "Failed to spawn zeroclaw daemon for '{}'",
                instance.name
            )
        })?;

    let pid = child.id();

    // Brief wait to catch immediate crashes (port conflict, bad config, missing deps)
    std::thread::sleep(std::time::Duration::from_millis(POST_SPAWN_CHECK_MS));
    match child.try_wait() {
        Ok(Some(exit_status)) => {
            // Child already exited -- startup failed
            let log_hint = log_path(inst_dir);
            return Err(LifecycleError::Internal(anyhow::anyhow!(
                "Daemon for '{}' exited immediately ({}). Check logs at {}",
                instance.name,
                exit_status,
                log_hint.display()
            )));
        }
        Ok(None) => {
            // Still running -- good
        }
        Err(e) => {
            // Can't check status -- kill and bail
            kill_child_best_effort(pid);
            return Err(LifecycleError::Internal(anyhow::anyhow!(
                "Failed to check daemon status after spawn for '{}': {e}",
                instance.name
            )));
        }
    }

    // Write PID and update status. If either fails, kill the orphan.
    if let Err(e) = write_pid(inst_dir, pid) {
        kill_child_best_effort(pid);
        return Err(LifecycleError::Internal(
            e.context("Failed to write PID file; killed spawned daemon"),
        ));
    }

    if let Err(e) = registry.update_status(&instance.id, "running") {
        kill_child_best_effort(pid);
        let _ = remove_pid(inst_dir); // best-effort cleanup
        return Err(LifecycleError::Internal(
            e.context("Failed to update DB status; killed spawned daemon"),
        ));
    }

    // Best-effort: cache PID in DB for supervisor queries
    if let Err(e) = registry.update_pid(&instance.id, Some(pid)) {
        tracing::warn!("Failed to cache PID in DB (non-fatal): {e:#}");
    }

    println!(
        "Started instance '{}' (PID {pid}, port {})",
        instance.name, instance.port
    );
    Ok(())
}

// ── Public API ─────────────────────────────────────────────────

/// Acquire the lifecycle lock, mapping the outcome to `LifecycleError`.
fn require_lifecycle_lock(inst_dir: &Path) -> Result<fs::File, LifecycleError> {
    match try_lifecycle_lock(inst_dir).map_err(LifecycleError::Internal)? {
        LockOutcome::Acquired(f) => Ok(f),
        LockOutcome::Contended => Err(LifecycleError::LockHeld),
    }
}

/// Start a registered instance by name.
pub fn start_instance(registry: &Registry, name: &str) -> Result<(), LifecycleError> {
    let instance = registry
        .get_instance_by_name(name)
        .map_err(LifecycleError::Internal)?
        .ok_or_else(|| LifecycleError::NotFound(name.to_string()))?;

    let inst_dir = instance_dir_from(&instance);
    let _lock = require_lifecycle_lock(&inst_dir)?;
    start_inner(registry, &instance, &inst_dir)
}

/// Stop a running instance by name.
pub fn stop_instance(registry: &Registry, name: &str) -> Result<(), LifecycleError> {
    let instance = registry
        .get_instance_by_name(name)
        .map_err(LifecycleError::Internal)?
        .ok_or_else(|| LifecycleError::NotFound(name.to_string()))?;

    let inst_dir = instance_dir_from(&instance);
    let _lock = require_lifecycle_lock(&inst_dir)?;
    stop_inner(registry, &instance, &inst_dir)
}

/// Restart an instance (stop if running, then start). Holds a single lifecycle
/// lock for the entire operation to prevent races.
pub fn restart_instance(registry: &Registry, name: &str) -> Result<(), LifecycleError> {
    let instance = registry
        .get_instance_by_name(name)
        .map_err(LifecycleError::Internal)?
        .ok_or_else(|| LifecycleError::NotFound(name.to_string()))?;

    let inst_dir = instance_dir_from(&instance);
    let _lock = require_lifecycle_lock(&inst_dir)?;

    // Stop if running
    if let Some(pid) = read_pid(&inst_dir)? {
        if is_pid_alive(pid) {
            stop_inner(registry, &instance, &inst_dir)?;
        } else {
            // Dead PID, just clean up
            remove_pid(&inst_dir)?;
            registry.update_status(&instance.id, "stopped")?;
        }
    }

    start_inner(registry, &instance, &inst_dir)
}

/// Determine live status of an instance from its PID file.
/// Returns (status_string, optional_pid).
pub fn live_status(instance_dir: &Path) -> Result<(String, Option<u32>)> {
    match read_pid(instance_dir)? {
        Some(pid) if is_pid_alive(pid) => {
            let owned = verify_pid_ownership(pid, instance_dir).unwrap_or(false);
            if owned {
                Ok(("running".to_string(), Some(pid)))
            } else {
                Ok(("stale-pid".to_string(), Some(pid)))
            }
        }
        Some(pid) => Ok(("dead".to_string(), Some(pid))),
        None => Ok(("stopped".to_string(), None)),
    }
}

/// Show status for all instances or a specific instance.
pub fn show_status(registry: &Registry, name: Option<&str>) -> Result<()> {
    if let Some(name) = name {
        let instance = registry
            .get_instance_by_name(name)?
            .ok_or_else(|| anyhow::anyhow!("No instance named '{name}'"))?;

        let inst_dir = instance_dir_from(&instance);
        let (status, pid) = live_status(&inst_dir)?;

        println!("Instance: {}", instance.name);
        println!("  ID:        {}", instance.id);
        println!("  Port:      {}", instance.port);
        println!("  Status:    {status}");
        if let Some(pid) = pid {
            println!("  PID:       {pid}");
        }
        println!("  Config:    {}", instance.config_path);
        if let Some(ws) = &instance.workspace_dir {
            println!("  Workspace: {ws}");
        }
    } else {
        let instances = registry.list_instances()?;
        if instances.is_empty() {
            println!("No instances registered.");
            return Ok(());
        }

        println!("{:<16} {:<8} {:<10} {:<8}", "NAME", "PORT", "STATUS", "PID");
        for inst in &instances {
            let inst_dir = instance_dir_from(inst);
            let (status, pid) = live_status(&inst_dir)?;
            let pid_str = pid.map_or("-".to_string(), |p| p.to_string());
            println!(
                "{:<16} {:<8} {:<10} {:<8}",
                inst.name, inst.port, status, pid_str
            );
        }
    }

    Ok(())
}

/// Show logs for an instance.
/// Reads the last `lines` lines from the daemon log file.
/// If `follow` is true, continues tailing new output (blocking).
pub fn show_logs(instance_dir: &Path, lines: usize, follow: bool) -> Result<()> {
    let path = log_path(instance_dir);
    if !path.exists() {
        bail!("No log file found at {}", path.display());
    }

    // Read last N lines
    let file =
        fs::File::open(&path).with_context(|| format!("Failed to open log: {}", path.display()))?;
    let reader = BufReader::new(&file);
    let all_lines: Vec<String> = reader.lines().filter_map(Result::ok).collect();
    let start = all_lines.len().saturating_sub(lines);
    for line in &all_lines[start..] {
        println!("{line}");
    }

    if follow {
        // Seek to end and poll for new data
        let mut file = fs::File::open(&path)?;
        file.seek(SeekFrom::End(0))?;
        let mut reader = BufReader::new(file);
        loop {
            let mut line = String::new();
            match reader.read_line(&mut line) {
                Ok(0) => {
                    std::thread::sleep(std::time::Duration::from_millis(500));
                }
                Ok(_) => {
                    print!("{line}");
                }
                Err(e) => {
                    bail!("Error reading log: {e}");
                }
            }
        }
    }

    Ok(())
}

/// Get the instance directory for a named instance (for use by CLI).
pub fn resolve_instance_dir(registry: &Registry, name: &str) -> Result<PathBuf> {
    let instance = registry
        .get_instance_by_name(name)?
        .ok_or_else(|| anyhow::anyhow!("No instance named '{name}'"))?;
    Ok(instance_dir_from(&instance))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn pid_roundtrip() {
        let tmp = TempDir::new().unwrap();
        assert!(read_pid(tmp.path()).unwrap().is_none());

        write_pid(tmp.path(), 12345).unwrap();
        assert_eq!(read_pid(tmp.path()).unwrap(), Some(12345));

        remove_pid(tmp.path()).unwrap();
        assert!(read_pid(tmp.path()).unwrap().is_none());
    }

    #[test]
    fn remove_pid_idempotent() {
        let tmp = TempDir::new().unwrap();
        remove_pid(tmp.path()).unwrap();
        remove_pid(tmp.path()).unwrap();
    }

    #[test]
    fn current_process_is_alive() {
        let pid = std::process::id();
        assert!(is_pid_alive(pid));
    }

    #[test]
    fn dead_pid_is_not_alive() {
        // PID 4294967 is extremely unlikely to be alive
        assert!(!is_pid_alive(4_294_967));
    }

    #[test]
    fn is_pid_alive_detects_eperm_as_alive() {
        // PID 1 (init/systemd) is always alive but we can't signal it as non-root.
        // On Linux as non-root, kill(1, 0) returns -1 with errno EPERM.
        // This test verifies we treat EPERM as "alive".
        if unsafe { libc::getuid() } != 0 {
            assert!(
                is_pid_alive(1),
                "PID 1 should be detected as alive even with EPERM"
            );
        }
    }

    #[test]
    fn ownership_wrong_dir_returns_false() {
        let pid = std::process::id();
        let result = verify_pid_ownership(pid, Path::new("/nonexistent/path")).unwrap();
        assert!(!result);
    }

    #[test]
    fn ownership_dead_pid_returns_false() {
        let result = verify_pid_ownership(4_294_967, Path::new("/any/path")).unwrap();
        assert!(!result);
    }

    #[test]
    fn ownership_unreadable_environ_returns_false() {
        // PID 1 environ is typically unreadable as non-root.
        // verify_pid_ownership should return false (safe default), not error.
        if unsafe { libc::getuid() } != 0 {
            let result = verify_pid_ownership(1, Path::new("/any/path")).unwrap();
            assert!(
                !result,
                "Unreadable environ should return false, not claim ownership"
            );
        }
    }

    #[test]
    fn lifecycle_lock_blocks_concurrent_access() {
        let tmp = TempDir::new().unwrap();
        let _lock1 = acquire_lifecycle_lock(tmp.path()).unwrap();
        let result = acquire_lifecycle_lock(tmp.path());
        assert!(result.is_err());
        assert!(
            result.unwrap_err().to_string().contains("Lifecycle lock held")
        );
    }

    #[test]
    fn lifecycle_lock_released_on_drop() {
        let tmp = TempDir::new().unwrap();
        {
            let _lock1 = acquire_lifecycle_lock(tmp.path()).unwrap();
        }
        let _lock2 = acquire_lifecycle_lock(tmp.path()).unwrap();
    }

    #[test]
    fn rotate_logs_creates_rotated_file() {
        let tmp = TempDir::new().unwrap();
        let logs = tmp.path().join(LOG_DIR);
        fs::create_dir_all(&logs).unwrap();

        let log = log_path(tmp.path());
        fs::write(&log, "line 1\nline 2\n").unwrap();

        rotate_logs(tmp.path()).unwrap();

        assert!(!log.exists());
        let rotated = rotated_log_path(tmp.path());
        assert_eq!(fs::read_to_string(rotated).unwrap(), "line 1\nline 2\n");
    }

    #[test]
    fn rotate_logs_noop_when_no_log() {
        let tmp = TempDir::new().unwrap();
        rotate_logs(tmp.path()).unwrap();
    }

    #[test]
    fn instance_dir_from_config_path() {
        let inst = Instance {
            id: "id-1".into(),
            name: "test".into(),
            status: "stopped".into(),
            port: 18801,
            config_path: "/home/user/.zeroclaw/cp/instances/abc-123/config.toml".into(),
            workspace_dir: None,
            archived_at: None,
            migration_run_id: None,
            pid: None,
        };
        let dir = instance_dir_from(&inst);
        assert_eq!(
            dir,
            PathBuf::from("/home/user/.zeroclaw/cp/instances/abc-123")
        );
    }

    #[test]
    fn live_status_stopped_when_no_pid() {
        let tmp = TempDir::new().unwrap();
        let (status, pid) = live_status(tmp.path()).unwrap();
        assert_eq!(status, "stopped");
        assert!(pid.is_none());
    }

    #[test]
    fn live_status_dead_when_stale_pid() {
        let tmp = TempDir::new().unwrap();
        write_pid(tmp.path(), 4_294_967).unwrap();
        let (status, pid) = live_status(tmp.path()).unwrap();
        assert_eq!(status, "dead");
        assert_eq!(pid, Some(4_294_967));
    }

    #[test]
    fn show_logs_reads_last_n_lines() {
        let tmp = TempDir::new().unwrap();
        let logs = tmp.path().join(LOG_DIR);
        fs::create_dir_all(&logs).unwrap();

        let content: String = (1..=100).map(|i| format!("line {i}\n")).collect();
        fs::write(log_path(tmp.path()), content).unwrap();

        show_logs(tmp.path(), 5, false).unwrap();
    }

    #[test]
    fn show_logs_errors_when_no_file() {
        let tmp = TempDir::new().unwrap();
        let result = show_logs(tmp.path(), 10, false);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No log file"));
    }
}
