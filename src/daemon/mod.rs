use anyhow::Result;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::signal;
use tokio::signal::unix::{signal, SignalKind};

/// Daemon configuration.
pub struct DaemonConfig {
    /// PID file location.
    pub pid_file: PathBuf,
    /// Unix socket for IPC.
    pub socket_path: PathBuf,
    /// Log directory.
    pub log_dir: PathBuf,
    /// Heartbeat file (written every 30s for watchdog).
    pub heartbeat_path: PathBuf,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        let home = dirs_home();
        let app_support = home.join("Library/Application Support/Augusta");
        Self {
            pid_file: app_support.join("augusta.pid"),
            socket_path: app_support.join("augusta.sock"),
            log_dir: home.join("Library/Logs/Augusta"),
            heartbeat_path: app_support.join("heartbeat"),
        }
    }
}

/// Daemon exit codes.
pub mod exit_code {
    /// Intentional shutdown (daemon stop) — launchd does NOT restart.
    pub const SUCCESS: i32 = 0;
    /// Crash or panic — launchd restarts.
    pub const CRASH: i32 = 1;
    /// Fatal config error — launchd restarts (retries with ThrottleInterval).
    pub const CONFIG_ERROR: i32 = 2;
}

/// Daemon entry point — runs the main event loop.
pub async fn run_daemon(config: DaemonConfig) -> Result<()> {
    // Install panic handler — ensures non-zero exit on panic so launchd restarts
    std::panic::set_hook(Box::new(|info| {
        eprintln!("Augusta daemon panicked: {info}");
        tracing::error!("PANIC: {info}");
        std::process::exit(exit_code::CRASH);
    }));

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

    // Start health monitor
    let monitor = Arc::new(crate::health::monitor::HealthMonitor::new());
    monitor.register("daemon".into(), "system".into()).await;
    monitor.record_ping("daemon").await;

    let heartbeat_path = config.heartbeat_path.clone();
    let monitor_for_loop = Arc::clone(&monitor);
    let health_handle = tokio::spawn(heartbeat_loop(monitor_for_loop, heartbeat_path));

    // Signal handlers
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to register SIGTERM handler");
    let mut sighup = signal(SignalKind::hangup()).expect("failed to register SIGHUP handler");

    // IPC + shutdown select loop
    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _addr)) => {
                        let t = Arc::clone(&start_time);
                        let m = Arc::clone(&monitor);
                        let hb = config.heartbeat_path.clone();
                        tokio::spawn(handle_ipc_client(stream, t, m, hb));
                    }
                    Err(e) => {
                        tracing::warn!("IPC accept error: {e}");
                    }
                }
            }
            _ = signal::ctrl_c() => {
                tracing::info!("Received SIGINT — shutting down");
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("Received SIGTERM — shutting down");
                break;
            }
            _ = sighup.recv() => {
                tracing::info!("Received SIGHUP — config reload not yet implemented");
            }
        }
    }

    health_handle.abort();

    tracing::info!("Shutting down Augusta daemon");
    crate::tui::event_bus::emit("daemon", "system", "agent_stopped", "Daemon stopped");

    // Cleanup
    let _ = std::fs::remove_file(&config.pid_file);
    let _ = std::fs::remove_file(&config.socket_path);
    let _ = std::fs::remove_file(&config.heartbeat_path);

    Ok(())
}

/// Handle a single IPC client connection.
///
/// Protocol: newline-delimited commands. Each command gets a JSON response.
/// Supported commands: `ping`, `status`, `version`, `health`.
pub async fn handle_ipc_client(
    stream: tokio::net::UnixStream,
    start_time: Arc<Instant>,
    monitor: Arc<crate::health::monitor::HealthMonitor>,
    heartbeat_path: PathBuf,
) {
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
            "health" => build_health_response(&monitor, &heartbeat_path).await,
            cmd => serde_json::json!({
                "status": "error",
                "error": format!("Unknown command: {cmd}"),
                "available": ["ping", "status", "version", "health"],
            }),
        };

        let mut response_bytes = serde_json::to_vec(&response).unwrap_or_default();
        response_bytes.push(b'\n');
        if writer.write_all(&response_bytes).await.is_err() {
            break;
        }
    }
}

/// Build the JSON response for the `health` IPC command.
async fn build_health_response(
    monitor: &crate::health::monitor::HealthMonitor,
    heartbeat_path: &std::path::Path,
) -> serde_json::Value {
    let snapshot = monitor.snapshot().await;
    let dead = monitor.dead_agents().await;
    let stuck = monitor.stuck_agents().await;

    let agents: Vec<serde_json::Value> = snapshot
        .iter()
        .map(|h| {
            serde_json::json!({
                "name": h.name,
                "role": h.role,
                "status": format!("{:?}", h.status),
                "kill_count": h.kill_count,
            })
        })
        .collect();

    let heartbeat_age_secs = heartbeat_age(heartbeat_path).await.unwrap_or(u64::MAX);

    let monitor_status = if !dead.is_empty() {
        "critical"
    } else if !stuck.is_empty() || heartbeat_age_secs > 120 {
        "degraded"
    } else {
        "healthy"
    };

    serde_json::json!({
        "status": "ok",
        "monitor_status": monitor_status,
        "heartbeat_age_secs": heartbeat_age_secs,
        "agents": agents,
        "dead_agents": dead,
        "stuck_agents": stuck,
    })
}

/// Read heartbeat file and return age in seconds.
async fn heartbeat_age(path: &std::path::Path) -> Option<u64> {
    let content = tokio::fs::read_to_string(path).await.ok()?;
    let ts: u64 = content.trim().parse().ok()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    Some(now.saturating_sub(ts))
}

/// Heartbeat loop — pings HealthMonitor, writes heartbeat file, evaluates health every 30s.
async fn heartbeat_loop(
    monitor: Arc<crate::health::monitor::HealthMonitor>,
    heartbeat_path: PathBuf,
) {
    use std::time::Duration;

    let mut interval = tokio::time::interval(Duration::from_secs(30));
    loop {
        interval.tick().await;

        // Record daemon ping
        monitor.record_ping("daemon").await;
        monitor.record_activity("daemon").await;

        // Write heartbeat timestamp
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let _ = tokio::fs::write(&heartbeat_path, format!("{ts}\n")).await;

        // Evaluate all registered agents
        monitor
            .evaluate_all(
                Duration::from_secs(90),  // ping timeout
                3,                        // max consecutive failures
                Duration::from_secs(300), // stuck threshold
            )
            .await;

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

    // Bootout existing service first (ignore errors if not loaded)
    let _ = launchctl_bootout(&plist_dest);

    if plist_source.exists() {
        std::fs::copy(&plist_source, &plist_dest)?;
    } else {
        let plist_content = generate_plist()?;
        std::fs::write(&plist_dest, plist_content)?;
    }

    // Bootstrap the agent (modern launchctl)
    let plist_str = plist_dest.to_str().unwrap_or_default();
    let domain_target = format!("gui/{}", unsafe { libc::getuid() });
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain_target, plist_str])
        .status()?;

    if status.success() {
        tracing::info!("Installed and bootstrapped com.lightwave.augusta");
    } else {
        // Fall back to legacy load for older macOS
        let fallback = std::process::Command::new("launchctl")
            .args(["load", plist_str])
            .status()?;
        if fallback.success() {
            tracing::info!("Installed com.lightwave.augusta (legacy load)");
        } else {
            anyhow::bail!("launchctl bootstrap/load failed");
        }
    }

    // Install watchdog
    install_watchdog()?;

    Ok(())
}

/// Install the watchdog launchd agent.
fn install_watchdog() -> Result<()> {
    let source = watchdog_plist_path_source();
    let dest = watchdog_plist_path_dest();

    if !source.exists() {
        tracing::debug!("Watchdog plist not found at source, skipping");
        return Ok(());
    }

    let _ = launchctl_bootout(&dest);
    std::fs::copy(&source, &dest)?;

    let dest_str = dest.to_str().unwrap_or_default();
    let domain_target = format!("gui/{}", unsafe { libc::getuid() });
    let status = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain_target, dest_str])
        .status()?;

    if !status.success() {
        let _ = std::process::Command::new("launchctl")
            .args(["load", dest_str])
            .status();
    }

    tracing::info!("Installed watchdog agent");
    Ok(())
}

/// Uninstall the launchd plist and watchdog.
pub fn uninstall_launchd() -> Result<()> {
    let plist_dest = plist_path_dest();
    if plist_dest.exists() {
        let _ = launchctl_bootout(&plist_dest);
        std::fs::remove_file(&plist_dest)?;
        tracing::info!("Uninstalled com.lightwave.augusta");
    }

    let watchdog_dest = watchdog_plist_path_dest();
    if watchdog_dest.exists() {
        let _ = launchctl_bootout(&watchdog_dest);
        std::fs::remove_file(&watchdog_dest)?;
        tracing::info!("Uninstalled watchdog agent");
    }

    Ok(())
}

/// Bootout a launchd service using modern launchctl, falling back to legacy unload.
fn launchctl_bootout(plist_path: &std::path::Path) -> Result<()> {
    let plist_str = plist_path.to_str().unwrap_or_default();
    let domain_target = format!("gui/{}", unsafe { libc::getuid() });
    let status = std::process::Command::new("launchctl")
        .args(["bootout", &domain_target, plist_str])
        .status()?;

    if !status.success() {
        // Fall back to legacy unload
        std::process::Command::new("launchctl")
            .args(["unload", plist_str])
            .status()?;
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
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("com.lightwave.augusta.plist")
}

fn watchdog_plist_path_source() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("com.lightwave.augusta.watchdog.plist")
}

fn plist_path_dest() -> PathBuf {
    dirs_home().join("Library/LaunchAgents/com.lightwave.augusta.plist")
}

fn watchdog_plist_path_dest() -> PathBuf {
    dirs_home().join("Library/LaunchAgents/com.lightwave.augusta.watchdog.plist")
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
    <dict>
        <key>SuccessfulExit</key>
        <false/>
    </dict>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>ExitTimeOut</key>
    <integer>30</integer>
    <key>ProcessType</key>
    <string>Standard</string>
    <key>SoftResourceLimits</key>
    <dict>
        <key>NumberOfFiles</key>
        <integer>4096</integer>
    </dict>
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
