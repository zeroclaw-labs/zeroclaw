//! Daemon stop tests — verify PID-based process signaling.

use lightwave_sys::daemon::{daemon_status, daemon_stop, DaemonConfig};

/// Stop when no PID file exists returns a clean message (not an error).
#[test]
fn daemon_stop_no_pid_file_returns_not_running() {
    // DaemonConfig::default() points at ~/Library/Application Support/Augusta/augusta.pid.
    // If no daemon is running, there's no PID file, and stop should report that.
    let result = daemon_stop();
    assert!(result.is_ok());
    let msg = result.unwrap();
    assert!(
        msg.contains("not running"),
        "Expected 'not running' message, got: {msg}"
    );
}

/// Status when no PID file exists returns not running.
#[test]
fn daemon_status_no_pid_file_returns_not_running() {
    let result = daemon_status();
    assert!(result.is_ok());
    let msg = result.unwrap();
    assert!(
        msg.contains("not running"),
        "Expected 'not running' in status, got: {msg}"
    );
}

/// DaemonConfig default paths are under ~/Library.
#[test]
fn daemon_config_default_paths_are_macos_standard() {
    let config = DaemonConfig::default();
    let pid = config.pid_file.to_string_lossy();
    let sock = config.socket_path.to_string_lossy();
    let log = config.log_dir.to_string_lossy();

    assert!(
        pid.contains("Library/Application Support/Augusta"),
        "PID file should be in Application Support: {pid}"
    );
    assert!(
        sock.contains("Library/Application Support/Augusta"),
        "Socket should be in Application Support: {sock}"
    );
    assert!(
        log.contains("Library/Logs/Augusta"),
        "Log dir should be in Library/Logs: {log}"
    );
}
