use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
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

    crate::tui::event_bus::emit("daemon", "system", "agent_started", "Daemon started");

    // Remove stale socket
    let _ = std::fs::remove_file(&config.socket_path);

    // Start Unix socket listener for IPC
    let listener = UnixListener::bind(&config.socket_path)?;
    tracing::info!("IPC socket bound: {}", config.socket_path.display());

    let start_time = Arc::new(Instant::now());

    // Start health monitor loop
    let health_handle = tokio::spawn(health_monitor_loop());

    // IPC + shutdown select loop
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let t = Arc::clone(&start_time);
                        tokio::spawn(handle_ipc_client(stream, t));
                    }
                    Err(e) => {
                        tracing::warn!("IPC accept error: {e}");
                    }
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("Received shutdown signal");
                break;
            }
        }
    }

    health_handle.abort();

    tracing::info!("Shutting down Augusta daemon");
    crate::tui::event_bus::emit("daemon", "system", "agent_stopped", "Daemon stopped");

    // Cleanup
    let _ = std::fs::remove_file(&config.pid_file);
    let _ = std::fs::remove_file(&config.socket_path);

    Ok(())
}

/// Handle a single IPC client connection.
///
/// Protocol: newline-delimited commands. Each command gets a JSON response.
/// Supported commands: `ping`, `status`, `version`.
async fn handle_ipc_client(stream: tokio::net::UnixStream, start_time: Arc<Instant>) {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();

    while let Ok(Some(line)) = lines.next_line().await {
        let response = match line.trim() {
            "ping" => serde_json::json!({
                "status": "ok",
                "pong": true,
                "pid": std::process::id(),
            }),
            "status" => serde_json::json!({
                "status": "ok",
                "pid": std::process::id(),
                "uptime_secs": start_time.elapsed().as_secs(),
                "version": env!("CARGO_PKG_VERSION"),
            }),
            "version" => serde_json::json!({
                "status": "ok",
                "version": env!("CARGO_PKG_VERSION"),
            }),
            cmd => serde_json::json!({
                "status": "error",
                "error": format!("Unknown command: {cmd}"),
                "available": ["ping", "status", "version"],
            }),
        };

        let mut response_bytes = serde_json::to_vec(&response).unwrap_or_default();
        response_bytes.push(b'\n');
        if writer.write_all(&response_bytes).await.is_err() {
            break;
        }
    }
}

/// Periodic health check loop (emits ping events every 60 seconds).
async fn health_monitor_loop() {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(60));
    loop {
        interval.tick().await;
        crate::tui::event_bus::emit("daemon", "system", "ping_success", "Health check OK");
    }
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

/// Query daemon status — tries Unix socket first for rich info, falls back to PID file.
pub fn daemon_status() -> Result<String> {
    let config = DaemonConfig::default();

    // Try socket-based status first (richer: uptime, version)
    if config.socket_path.exists() {
        if let Ok(info) = query_daemon_socket(&config.socket_path) {
            return Ok(info);
        }
    }

    // Fall back to PID file check
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

/// Connect to daemon socket and query live status.
fn query_daemon_socket(socket_path: &std::path::Path) -> Result<String> {
    use std::io::{BufRead, Write};
    use std::os::unix::net::UnixStream;

    let mut stream =
        UnixStream::connect(socket_path).map_err(|e| anyhow::anyhow!("socket connect: {e}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;

    stream.write_all(b"status\n")?;
    stream.flush()?;

    let mut reader = std::io::BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line)?;

    let resp: serde_json::Value =
        serde_json::from_str(line.trim()).map_err(|e| anyhow::anyhow!("parse: {e}"))?;

    if resp["status"] != "ok" {
        anyhow::bail!("daemon returned error");
    }

    let pid = resp["pid"].as_u64().unwrap_or(0);
    let uptime = resp["uptime_secs"].as_u64().unwrap_or(0);
    let version = resp["version"].as_str().unwrap_or("unknown");

    let uptime_str = format_uptime(uptime);
    Ok(format!(
        "Augusta daemon is running (PID: {pid}, uptime: {uptime_str}, v{version})"
    ))
}

fn format_uptime(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else if secs < 86400 {
        format!("{}h {}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d {}h", secs / 86400, (secs % 86400) / 3600)
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
