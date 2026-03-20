use anyhow::Result;
use std::path::PathBuf;
use tokio::signal;

/// Daemon configuration.
pub struct DaemonConfig {
    /// PID file location.
    pub pid_file: PathBuf,
    /// Unix socket for IPC.
    pub socket_path: PathBuf,
    /// Log directory.
    pub log_dir: PathBuf,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let home = dirs_home();
        Self {
            pid_file: home.join("Library/Application Support/Augusta/augusta.pid"),
            socket_path: home.join("Library/Application Support/Augusta/augusta.sock"),
            log_dir: home.join("Library/Logs/Augusta"),
        }
    }
}

/// Daemon lifecycle actions.
pub enum DaemonAction {
    Start,
    Stop,
    Status,
    Install,
    Uninstall,
}

/// Daemon entry point — runs the main event loop.
pub async fn run_daemon(config: DaemonConfig) -> Result<()> {
    // Ensure directories exist
    if let Some(parent) = config.pid_file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir_all(&config.log_dir)?;

    // Write PID file
    let pid = std::process::id();
    std::fs::write(&config.pid_file, pid.to_string())?;

    tracing::info!("Augusta daemon started (PID: {pid})");
    tracing::info!("Socket: {}", config.socket_path.display());

    // TODO: Start Unix socket listener for IPC
    // TODO: Start health monitor loop
    // TODO: Start FSEvents watcher
    // TODO: Start service registry

    // Wait for shutdown signal
    signal::ctrl_c().await?;

    tracing::info!("Shutting down Augusta daemon");

    // Cleanup PID file
    let _ = std::fs::remove_file(&config.pid_file);
    let _ = std::fs::remove_file(&config.socket_path);

    Ok(())
}

/// Install the launchd plist for auto-start.
pub fn install_launchd() -> Result<()> {
    let plist_source = plist_path_source();
    let plist_dest = plist_path_dest();

    if let Some(parent) = plist_dest.parent() {
        std::fs::create_dir_all(parent)?;
    }

    if plist_source.exists() {
        std::fs::copy(&plist_source, &plist_dest)?;
    } else {
        // Generate default plist
        let plist_content = generate_plist()?;
        std::fs::write(&plist_dest, plist_content)?;
    }

    // Load the agent
    let status = std::process::Command::new("launchctl")
        .args(["load", plist_dest.to_str().unwrap_or_default()])
        .status()?;

    if status.success() {
        tracing::info!("Installed and loaded com.lightwave.augusta");
    } else {
        anyhow::bail!("launchctl load failed with exit code: {}", status);
    }

    Ok(())
}

/// Uninstall the launchd plist.
pub fn uninstall_launchd() -> Result<()> {
    let plist_dest = plist_path_dest();

    if plist_dest.exists() {
        let _ = std::process::Command::new("launchctl")
            .args(["unload", plist_dest.to_str().unwrap_or_default()])
            .status();
        std::fs::remove_file(&plist_dest)?;
        tracing::info!("Uninstalled com.lightwave.augusta");
    }

    Ok(())
}

/// Stop the daemon by sending SIGTERM to the PID in the PID file.
pub fn daemon_stop() -> Result<String> {
    let config = DaemonConfig::default();

    if !config.pid_file.exists() {
        return Ok("Augusta daemon is not running (no PID file)".into());
    }

    let pid_str = std::fs::read_to_string(&config.pid_file)?;
    let pid: i32 = pid_str.trim().parse()?;

    // Check if process is alive before sending signal
    let alive = unsafe { libc::kill(pid, 0) == 0 };
    if !alive {
        let _ = std::fs::remove_file(&config.pid_file);
        return Ok("Augusta daemon is not running (stale PID file cleaned)".into());
    }

    // Send SIGTERM for graceful shutdown
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result != 0 {
        anyhow::bail!(
            "Failed to send SIGTERM to PID {pid}: {}",
            std::io::Error::last_os_error()
        );
    }

    // Clean up PID file (daemon also cleans up on exit, but be safe)
    let _ = std::fs::remove_file(&config.pid_file);

    Ok(format!("Augusta daemon stopped (PID: {pid})"))
}

/// Query daemon status via PID file.
pub fn daemon_status() -> Result<String> {
    let config = DaemonConfig::default();

    if !config.pid_file.exists() {
        return Ok("Augusta daemon is not running".into());
    }

    let pid_str = std::fs::read_to_string(&config.pid_file)?;
    let pid: u32 = pid_str.trim().parse()?;

    // Check if process is alive
    let alive = unsafe { libc::kill(pid as i32, 0) == 0 };

    if alive {
        Ok(format!("Augusta daemon is running (PID: {pid})"))
    } else {
        // Stale PID file
        let _ = std::fs::remove_file(&config.pid_file);
        Ok("Augusta daemon is not running (stale PID file cleaned)".into())
    }
}

fn dirs_home() -> PathBuf {
    dirs_sys_home().unwrap_or_else(|| PathBuf::from("/tmp"))
}

fn dirs_sys_home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn plist_path_source() -> PathBuf {
    // Relative to the package root
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("com.lightwave.augusta.plist")
}

fn plist_path_dest() -> PathBuf {
    dirs_home().join("Library/LaunchAgents/com.lightwave.augusta.plist")
}

fn generate_plist() -> Result<String> {
    let augusta_bin = which::which("augusta")
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "/usr/local/bin/augusta".into());

    let log_dir = dirs_home().join("Library/Logs/Augusta");

    Ok(format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.lightwave.augusta</string>
    <key>ProgramArguments</key>
    <array>
        <string>{augusta_bin}</string>
        <string>daemon</string>
        <string>start</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>StandardOutPath</key>
    <string>{stdout}</string>
    <key>StandardErrorPath</key>
    <string>{stderr}</string>
</dict>
</plist>"#,
        stdout = log_dir.join("stdout.log").display(),
        stderr = log_dir.join("stderr.log").display(),
    ))
}
