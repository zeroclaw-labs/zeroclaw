use anyhow::{Context, Result, bail};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use daemonclaw_config::schema::Config;

const SERVICE_LABEL: &str = "com.daemonclaw.daemon";
const WINDOWS_TASK_NAME: &str = "DaemonClaw Daemon";

/// Supported init systems for service management
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum InitSystem {
    /// Auto-detect based on system indicators
    #[default]
    Auto,
    /// systemd (via systemctl --user)
    Systemd,
    /// OpenRC (via rc-service)
    Openrc,
}

impl FromStr for InitSystem {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s.to_lowercase().as_str() {
            "auto" => Ok(Self::Auto),
            "systemd" => Ok(Self::Systemd),
            "openrc" => Ok(Self::Openrc),
            other => bail!(
                "Unknown init system: '{}'. Supported: auto, systemd, openrc",
                other
            ),
        }
    }
}

impl InitSystem {
    /// Resolve auto-detection to a concrete init system
    ///
    /// Detection order (deny-by-default):
    /// 1. `/run/systemd/system` exists → Systemd
    /// 2. `/run/openrc` exists AND OpenRC binary present → OpenRC
    /// 3. else → Error (unknown init system)
    #[cfg(target_os = "linux")]
    pub fn resolve(self) -> Result<Self> {
        match self {
            Self::Auto => detect_init_system(),
            concrete => Ok(concrete),
        }
    }

    #[cfg(not(target_os = "linux"))]
    pub fn resolve(self) -> Result<Self> {
        match self {
            Self::Auto => Ok(Self::Systemd),
            concrete => Ok(concrete),
        }
    }
}

/// Detect the active init system on Linux
///
/// Checks for systemd and OpenRC in order, returning the first match.
/// Returns an error if neither is detected.
#[cfg(target_os = "linux")]
fn detect_init_system() -> Result<InitSystem> {
    // Check for systemd first (most common on modern Linux)
    if Path::new("/run/systemd/system").exists() {
        return Ok(InitSystem::Systemd);
    }

    // Check for OpenRC: requires /run/openrc AND openrc binary
    if Path::new("/run/openrc").exists() {
        // Check for OpenRC binaries: /sbin/openrc-run or rc-service in PATH
        if Path::new("/sbin/openrc-run").exists() || which::which("rc-service").is_ok() {
            return Ok(InitSystem::Openrc);
        }
    }

    bail!(
        "Could not detect init system. Supported: systemd, OpenRC. \
         Use --service-init to specify manually."
    );
}

fn windows_task_name() -> &'static str {
    WINDOWS_TASK_NAME
}

/// Returns whether the DaemonClaw daemon service is currently running.
pub fn is_running() -> bool {
    if cfg!(target_os = "macos") {
        run_capture(Command::new("launchctl").arg("list"))
            .map(|out| out.lines().any(|l| l.contains(SERVICE_LABEL)))
            .unwrap_or(false)
    } else if cfg!(target_os = "linux") {
        is_running_linux()
    } else if cfg!(target_os = "windows") {
        run_capture(Command::new("schtasks").args([
            "/Query",
            "/TN",
            WINDOWS_TASK_NAME,
            "/FO",
            "LIST",
        ]))
        .map(|out| out.contains("Running"))
        .unwrap_or(false)
    } else {
        false
    }
}

fn is_running_linux() -> bool {
    if run_capture(Command::new("systemctl").args(["is-active", "daemonclaw.service"]))
        .map(|out| out.trim() == "active")
        .unwrap_or(false)
    {
        return true;
    }
    run_capture(Command::new("rc-service").args(["daemonclaw", "status"]))
        .map(|out| out.contains("started"))
        .unwrap_or(false)
}

pub fn install(init_system: InitSystem, dry_run: bool) -> Result<()> {
    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        install_linux(resolved, dry_run)
    } else {
        bail!(
            "daemonclaw service install currently supports Linux only. \
             macOS and Windows support is disabled."
        );
    }
}

pub fn start(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        // Ensure the Homebrew var directory exists before launchd tries to use it.
        // The plist may reference this path for WorkingDirectory and log files.
        let exe = std::env::current_exe().ok();
        if let Some(ref exe_path) = exe
            && let Some(var_dir) = detect_homebrew_var_dir(exe_path)
        {
            let _ = fs::create_dir_all(&var_dir);
        }
        let plist = macos_service_file()?;
        run_checked(Command::new("launchctl").arg("load").arg("-w").arg(&plist))?;
        run_checked(Command::new("launchctl").arg("start").arg(SERVICE_LABEL))?;
        println!("✅ Service started");
        Ok(())
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        start_linux(resolved)
    } else if cfg!(target_os = "windows") {
        let _ = config;
        run_checked(Command::new("schtasks").args(["/Run", "/TN", windows_task_name()]))?;
        println!("✅ Service started");
        Ok(())
    } else {
        let _ = config;
        anyhow::bail!("Service management is supported on macOS and Linux only")
    }
}

fn start_linux(init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            run_checked(Command::new("systemctl").args(["daemon-reload"]))?;
            run_checked(Command::new("systemctl").args(["start", "daemonclaw.service"]))?;
        }
        InitSystem::Openrc => {
            run_checked(Command::new("rc-service").args(["daemonclaw", "start"]))?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service started");
    Ok(())
}

pub fn stop(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        let plist = macos_service_file()?;
        let _ = run_checked(Command::new("launchctl").arg("stop").arg(SERVICE_LABEL));
        let _ = run_checked(
            Command::new("launchctl")
                .arg("unload")
                .arg("-w")
                .arg(&plist),
        );
        println!("✅ Service stopped");
        Ok(())
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        stop_linux(resolved)
    } else if cfg!(target_os = "windows") {
        let _ = config;
        let task_name = windows_task_name();
        let _ = run_checked(Command::new("schtasks").args(["/End", "/TN", task_name]));
        println!("✅ Service stopped");
        Ok(())
    } else {
        let _ = config;
        anyhow::bail!("Service management is supported on macOS and Linux only")
    }
}

fn stop_linux(init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let _ =
                run_checked(Command::new("systemctl").args(["stop", "daemonclaw.service"]));
        }
        InitSystem::Openrc => {
            let _ = run_checked(Command::new("rc-service").args(["daemonclaw", "stop"]));
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service stopped");
    Ok(())
}

pub fn restart(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        stop(config, init_system)?;
        start(config, init_system)?;
        println!("✅ Service restarted");
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return restart_linux(resolved);
    }

    if cfg!(target_os = "windows") {
        stop(config, init_system)?;
        start(config, init_system)?;
        println!("✅ Service restarted");
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn restart_linux(init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            run_checked(Command::new("systemctl").args(["daemon-reload"]))?;
            run_checked(Command::new("systemctl").args(["restart", "daemonclaw.service"]))?;
        }
        InitSystem::Openrc => {
            run_checked(Command::new("rc-service").args(["daemonclaw", "restart"]))?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service restarted");
    Ok(())
}

pub fn status(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        let out = run_capture(Command::new("launchctl").arg("list"))?;
        let running = out.lines().any(|line| line.contains(SERVICE_LABEL));
        println!(
            "Service: {}",
            if running {
                "✅ running/loaded"
            } else {
                "❌ not loaded"
            }
        );
        println!("Unit: {}", macos_service_file()?.display());
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return status_linux(config, resolved);
    }

    if cfg!(target_os = "windows") {
        let _ = config;
        let task_name = windows_task_name();
        let out =
            run_capture(Command::new("schtasks").args(["/Query", "/TN", task_name, "/FO", "LIST"]));
        match out {
            Ok(text) => {
                let running = text.contains("Running");
                println!(
                    "Service: {}",
                    if running {
                        "✅ running"
                    } else {
                        "❌ not running"
                    }
                );
                println!("Task: {}", task_name);
            }
            Err(_) => {
                println!("Service: ❌ not installed");
            }
        }
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn status_linux(_config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let out = run_capture(Command::new("systemctl").args([
                "is-active",
                "daemonclaw.service",
            ]))
            .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!("Unit: /etc/systemd/system/daemonclaw.service");
        }
        InitSystem::Openrc => {
            let out = run_capture(Command::new("rc-service").args(["daemonclaw", "status"]))
                .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!("Unit: /etc/init.d/daemonclaw");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

pub fn logs(config: &Config, init_system: InitSystem, lines: usize, follow: bool) -> Result<()> {
    if cfg!(target_os = "macos") {
        return logs_macos(config, lines, follow);
    }
    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return logs_linux(config, resolved, lines, follow);
    }
    if cfg!(target_os = "windows") {
        return logs_windows(config, lines, follow);
    }
    anyhow::bail!("Service log viewing is supported on macOS, Linux, and Windows only")
}

fn logs_macos(config: &Config, lines: usize, follow: bool) -> Result<()> {
    // Try the launchd log files first (StandardOutPath / StandardErrorPath from the plist).
    // These are the most reliable source since they capture all daemon output.
    let exe = std::env::current_exe().ok();
    let homebrew_var_dir = exe.as_ref().and_then(|e| detect_homebrew_var_dir(e));
    let logs_dir = if let Some(ref var_dir) = homebrew_var_dir {
        var_dir.join("logs")
    } else {
        config
            .config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join("logs")
    };

    let stderr_log = logs_dir.join("daemon.stderr.log");
    let stdout_log = logs_dir.join("daemon.stdout.log");

    // Prefer stderr log (most informative), fall back to stdout
    let log_file = if stderr_log.exists() {
        stderr_log
    } else if stdout_log.exists() {
        stdout_log
    } else {
        bail!(
            "No log files found in {}. Is the service installed?",
            logs_dir.display()
        );
    };

    if follow {
        let status = Command::new("tail")
            .args(["-n", &lines.to_string(), "-f"])
            .arg(&log_file)
            .status()
            .context("Failed to run tail")?;
        if !status.success() {
            bail!("tail exited with non-zero status");
        }
    } else {
        let status = Command::new("tail")
            .args(["-n", &lines.to_string()])
            .arg(&log_file)
            .status()
            .context("Failed to run tail")?;
        if !status.success() {
            bail!("tail exited with non-zero status");
        }
    }
    Ok(())
}

fn logs_linux(config: &Config, init_system: InitSystem, lines: usize, follow: bool) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let mut args = vec![
                "-u".to_string(),
                "daemonclaw.service".to_string(),
                "-n".to_string(),
                lines.to_string(),
                "--no-pager".to_string(),
            ];
            if follow {
                args.push("-f".to_string());
            }
            let status = Command::new("journalctl")
                .args(&args)
                .status()
                .context("Failed to run journalctl")?;
            if !status.success() {
                bail!("journalctl exited with non-zero status");
            }
        }
        InitSystem::Openrc => {
            // OpenRC logs go to /var/log/daemonclaw/error.log (as configured in the init script)
            let log_file = Path::new("/var/log/daemonclaw/error.log");
            if !log_file.exists() {
                // Fall back to access log
                let access_log = Path::new("/var/log/daemonclaw/access.log");
                if !access_log.exists() {
                    bail!("No log files found at /var/log/daemonclaw/. Is the service installed?");
                }
                return tail_file(access_log, lines, follow);
            }
            tail_file(log_file, lines, follow)?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    let _ = config;
    Ok(())
}

fn logs_windows(config: &Config, lines: usize, follow: bool) -> Result<()> {
    let logs_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("logs");

    let stderr_log = logs_dir.join("daemon.stderr.log");
    let stdout_log = logs_dir.join("daemon.stdout.log");

    let log_file = if stderr_log.exists() {
        stderr_log
    } else if stdout_log.exists() {
        stdout_log
    } else {
        bail!(
            "No log files found in {}. Is the service installed?",
            logs_dir.display()
        );
    };

    if follow {
        // Windows: use PowerShell Get-Content -Wait for tail -f equivalent
        let status = Command::new("powershell")
            .args([
                "-Command",
                &format!(
                    "Get-Content -Path '{}' -Tail {} -Wait",
                    log_file.display(),
                    lines
                ),
            ])
            .status()
            .context("Failed to run PowerShell Get-Content")?;
        if !status.success() {
            bail!("PowerShell Get-Content exited with non-zero status");
        }
    } else {
        let status = Command::new("powershell")
            .args([
                "-Command",
                &format!("Get-Content -Path '{}' -Tail {}", log_file.display(), lines),
            ])
            .status()
            .context("Failed to run PowerShell Get-Content")?;
        if !status.success() {
            bail!("PowerShell Get-Content exited with non-zero status");
        }
    }
    Ok(())
}

/// Tail a log file using the system `tail` command.
fn tail_file(path: &Path, lines: usize, follow: bool) -> Result<()> {
    let mut args = vec!["-n".to_string(), lines.to_string()];
    if follow {
        args.push("-f".to_string());
    }
    let status = Command::new("tail")
        .args(&args)
        .arg(path)
        .status()
        .context("Failed to run tail")?;
    if !status.success() {
        bail!("tail exited with non-zero status");
    }
    Ok(())
}

pub fn uninstall(config: &Config, init_system: InitSystem) -> Result<()> {
    stop(config, init_system)?;

    if cfg!(target_os = "macos") {
        let file = macos_service_file()?;
        if file.exists() {
            fs::remove_file(&file)
                .with_context(|| format!("Failed to remove {}", file.display()))?;
        }
        println!("✅ Service uninstalled ({})", file.display());
        return Ok(());
    }

    if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        return uninstall_linux(config, resolved);
    }

    if cfg!(target_os = "windows") {
        let task_name = windows_task_name();
        let _ = run_checked(Command::new("schtasks").args(["/Delete", "/TN", task_name, "/F"]));
        // Remove the wrapper script
        let wrapper = config
            .config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join("logs")
            .join("daemonclaw-daemon.cmd");
        if wrapper.exists() {
            fs::remove_file(&wrapper).ok();
        }
        println!("✅ Service uninstalled");
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn uninstall_linux(_config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let file = Path::new("/etc/systemd/system/daemonclaw.service");
            if file.exists() {
                fs::remove_file(file)
                    .with_context(|| format!("Failed to remove {}", file.display()))?;
            }
            let _ = run_checked(Command::new("systemctl").args(["daemon-reload"]));
            println!("✅ Service uninstalled ({})", file.display());
        }
        InitSystem::Openrc => {
            let init_script = Path::new("/etc/init.d/daemonclaw");
            if init_script.exists() {
                if let Err(err) =
                    run_checked(Command::new("rc-update").args(["del", "daemonclaw", "default"]))
                {
                    eprintln!(
                        "⚠️  Warning: Could not remove daemonclaw from OpenRC default runlevel: {err}"
                    );
                }
                fs::remove_file(init_script)
                    .with_context(|| format!("Failed to remove {}", init_script.display()))?;
            }
            println!("✅ Service uninstalled (/etc/init.d/daemonclaw)");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

/// Detect if the executable lives under a Homebrew prefix and return the
/// corresponding `var/daemonclaw` directory.
///
/// Homebrew installs binaries into `<prefix>/Cellar/<formula>/<version>/bin/`
/// and symlinks them to `<prefix>/bin/`. The canonical `var` directory is
/// `<prefix>/var`.  We check for both layouts.
fn detect_homebrew_var_dir(exe: &Path) -> Option<PathBuf> {
    let path_str = exe.to_string_lossy();

    // Symlinked binary: <prefix>/bin/daemonclaw
    // Cellar binary:    <prefix>/Cellar/daemonclaw/<version>/bin/daemonclaw
    let prefix = if path_str.contains("/Cellar/") {
        // Walk up from .../Cellar/daemonclaw/<ver>/bin/daemonclaw to the prefix
        let mut ancestor = exe.to_path_buf();
        while let Some(parent) = ancestor.parent() {
            ancestor = parent.to_path_buf();
            if ancestor.file_name().is_some_and(|n| n == "Cellar") {
                // prefix is one level above Cellar
                return ancestor.parent().map(|p| p.join("var").join("daemonclaw"));
            }
        }
        return None;
    } else if let Some(bin_parent) = exe.parent() {
        // <prefix>/bin/daemonclaw → check if <prefix>/Cellar exists (Homebrew marker)
        if let Some(prefix) = bin_parent.parent() {
            if prefix.join("Cellar").is_dir() {
                Some(prefix.to_path_buf())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    };

    prefix.map(|p| p.join("var").join("daemonclaw"))
}

#[allow(dead_code)]
fn install_macos(config: &Config) -> Result<()> {
    let file = macos_service_file()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;

    // When installed via Homebrew, use the Homebrew var directory for runtime
    // data so that `brew services start daemonclaw` works out of the box.
    let homebrew_var_dir = detect_homebrew_var_dir(&exe);
    if let Some(ref var_dir) = homebrew_var_dir {
        fs::create_dir_all(var_dir).with_context(|| {
            format!(
                "Failed to create Homebrew var directory: {}",
                var_dir.display()
            )
        })?;
    }

    let logs_dir = if let Some(ref var_dir) = homebrew_var_dir {
        var_dir.join("logs")
    } else {
        config
            .config_path
            .parent()
            .map_or_else(|| PathBuf::from("."), PathBuf::from)
            .join("logs")
    };
    fs::create_dir_all(&logs_dir)?;

    let stdout = logs_dir.join("daemon.stdout.log");
    let stderr = logs_dir.join("daemon.stderr.log");

    // When running under Homebrew, inject DAEMONCLAW_CONFIG_DIR and
    // WorkingDirectory so the daemon finds its data in the Homebrew prefix.
    let env_section = if let Some(ref var_dir) = homebrew_var_dir {
        format!(
            r#"  <key>EnvironmentVariables</key>
  <dict>
    <key>DAEMONCLAW_CONFIG_DIR</key>
    <string>{config_dir}</string>
  </dict>
  <key>WorkingDirectory</key>
  <string>{working_dir}</string>
"#,
            config_dir = xml_escape(&var_dir.display().to_string()),
            working_dir = xml_escape(&var_dir.display().to_string()),
        )
    } else {
        String::new()
    };

    let plist = format!(
        r#"<?xml version=\"1.0\" encoding=\"UTF-8\"?>
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">
<plist version=\"1.0\">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
{env_section}  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        exe = xml_escape(&exe.display().to_string()),
        env_section = env_section,
        stdout = xml_escape(&stdout.display().to_string()),
        stderr = xml_escape(&stderr.display().to_string())
    );

    fs::write(&file, plist)?;
    println!("✅ Installed launchd service: {}", file.display());
    if let Some(ref var_dir) = homebrew_var_dir {
        println!("   Homebrew var: {}", var_dir.display());
    }
    println!("   Start with: daemonclaw service start");
    Ok(())
}

fn install_linux(init_system: InitSystem, dry_run: bool) -> Result<()> {
    match init_system {
        InitSystem::Systemd => install_linux_systemd(dry_run),
        InitSystem::Openrc => {
            bail!("daemonclaw service install supports systemd only.");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
}

// ── Access-matrix-compliant systemd installer ────────────────────────
// See ACCESS_MATRIX.md for the authoritative specification.
// Every ownership, mode, ACL, and systemd directive here maps to a
// specific row in that document.

const AGENTS_GROUP: &str = "agents";
const ADMIN_GROUP: &str = "daemonclaw-admin";
const SERVICE_USER: &str = "daemonclaw";
const HOME_DIR: &str = "/var/lib/daemonclaw";
const ETC_DIR: &str = "/etc/daemonclaw";
const BACKUP_DIR: &str = "/var/backups/daemonclaw";

fn run_install_cmd(cmd: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("Failed to run {cmd}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("{cmd} {} failed: {}", args.join(" "), stderr.trim());
    }
    Ok(())
}

fn group_exists_check(name: &str) -> bool {
    Command::new("getent")
        .args(["group", name])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn user_exists_check(name: &str) -> bool {
    Command::new("id")
        .arg(name)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn set_owner(path: &Path, owner: &str, group: &str) -> Result<()> {
    run_install_cmd("chown", &[&format!("{owner}:{group}"), &path.to_string_lossy()])
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))
            .with_context(|| format!("chmod {:04o} {}", mode, path.display()))?;
    }
    Ok(())
}

fn mkdir_owned(path: &Path, mode: u32, owner: &str, group: &str, dry_run: bool) -> Result<()> {
    if path.exists() {
        return Ok(());
    }
    if dry_run {
        println!("  [dry-run] [dir] would create {} ({owner}:{group} {:04o})", path.display(), mode);
        return Ok(());
    }
    fs::create_dir_all(path)
        .with_context(|| format!("mkdir {}", path.display()))?;
    set_mode(path, mode)?;
    set_owner(path, owner, group)?;
    println!("  [dir] {} ({owner}:{group} {:04o})", path.display(), mode);
    Ok(())
}

fn install_linux_systemd(dry_run: bool) -> Result<()> {
    if !dry_run && !is_root() {
        bail!(
            "daemonclaw service install requires root.\n\
             Run with: sudo daemonclaw service install"
        );
    }

    if !dry_run {
        let _ = Command::new("systemctl")
            .args(["stop", "daemonclaw.service"])
            .output();

        // Kill any stale process holding the gateway port.
        // The previous instance may not have released it yet after systemctl stop.
        let _ = Command::new("fuser")
            .args(["-k", "42617/tcp"])
            .output();

        // Brief pause for the port to be released by the kernel (TIME_WAIT).
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    let home = Path::new(HOME_DIR);
    let etc = Path::new(ETC_DIR);

    // ── Groups ──
    for name in [AGENTS_GROUP, ADMIN_GROUP] {
        if !group_exists_check(name) {
            if dry_run {
                println!("[dry-run] would create group: {name}");
            } else {
                run_install_cmd("groupadd", &["--system", name])?;
                println!("Created system group: {name}");
            }
        }
    }

    // ── Service user ──
    if !user_exists_check(SERVICE_USER) {
        if dry_run {
            println!("[dry-run] would create user: {SERVICE_USER}");
        } else {
            run_install_cmd("useradd", &[
                "--system",
                "--gid", AGENTS_GROUP,
                "--home-dir", HOME_DIR,
                "--create-home",
                "--shell", "/usr/sbin/nologin",
                SERVICE_USER,
            ])?;
            println!("Created system user: {SERVICE_USER}");
        }
    }

    // ── Config directory ──
    if !etc.exists() {
        if dry_run {
            println!("[dry-run] would create {}", etc.display());
        } else {
            fs::create_dir_all(etc)
                .with_context(|| format!("mkdir {}", etc.display()))?;
            set_mode(etc, 0o750)?;
            set_owner(etc, "root", AGENTS_GROUP)?;
            println!("Created directory: {}", etc.display());
        }
    }

    // ── Config file (migrate, validate existing, or generate fresh) ──
    let etc_config = etc.join("config.toml");
    let needs_config = if etc_config.exists() {
        let raw = fs::read_to_string(&etc_config)
            .with_context(|| format!("read {}", etc_config.display()))?;
        daemonclaw_config::migration::migrate_file(&raw).is_err()
    } else {
        true
    };
    if needs_config {
        if dry_run {
            println!("[dry-run] would write {}", etc_config.display());
        } else {
            if etc_config.exists() {
                let bak = etc.join("config.toml.bak");
                fs::rename(&etc_config, &bak)
                    .with_context(|| format!("backup {}", etc_config.display()))?;
                println!("Backed up broken config to {}", bak.display());
            }
            let content = match resolve_invoking_user_config() {
                Some(source) => {
                    let c = fs::read_to_string(&source)
                        .with_context(|| format!("read {}", source.display()))?;
                    println!("Migrated config from {}", source.display());
                    c
                }
                None => {
                    let c = generate_install_config();
                    println!("Generated default config");
                    c
                }
            };
            fs::write(&etc_config, &content)
                .with_context(|| format!("write {}", etc_config.display()))?;
            set_mode(&etc_config, 0o640)?;
            set_owner(&etc_config, "root", AGENTS_GROUP)?;
            run_install_cmd("setfacl", &["-m", &format!("g:{ADMIN_GROUP}:rw-"), &etc_config.to_string_lossy()])?;
        }
    }

    // ── Home directory structure ──
    let dirs: &[(&str, u32)] = &[
        (".daemonclaw",                    0o750),
        (".daemonclaw/workspace",          0o750),
        (".daemonclaw/workspace/github",   0o750),
        (".daemonclaw/workspace/skills",   0o750),
        (".daemonclaw/workspace/memory",   0o700),
        (".daemonclaw/workspace/sessions", 0o700),
        (".daemonclaw/state",              0o750),
        (".daemonclaw/state/backups",      0o750),
        (".daemonclaw/logs",               0o750),
        ("tmp",                            0o700),
    ];
    for (rel, mode) in dirs {
        let path = home.join(rel);
        if !path.exists() {
            if dry_run {
                println!("[dry-run] would create {}", path.display());
            } else {
                fs::create_dir_all(&path)
                    .with_context(|| format!("mkdir {}", path.display()))?;
                set_mode(&path, *mode)?;
                set_owner(&path, SERVICE_USER, AGENTS_GROUP)?;
            }
        }
    }

    // ── Default ACLs on logs/ and state/ ──
    if !dry_run {
        for rel in [".daemonclaw/logs", ".daemonclaw/state"] {
            let full = home.join(rel);
            let acl = format!("default:group:{AGENTS_GROUP}:r--");
            run_install_cmd("setfacl", &["-m", &acl, &full.to_string_lossy()])?;
        }
    }

    // ── Secret key ──
    // If we copied config from the invoking user, their .secret_key must also
    // be copied so the service can decrypt any enc2: values in that config.
    // Only generate a fresh key when no user key is available.
    let key_path = home.join(".daemonclaw/.secret_key");
    if !key_path.exists() {
        if dry_run {
            println!("[dry-run] would provision secret key");
        } else {
            let user_key = resolve_invoking_user_secret_key();
            if let Some(source_key) = user_key {
                fs::copy(&source_key, &key_path)
                    .with_context(|| format!("copy secret key from {}", source_key.display()))?;
                println!("Copied secret key from {}", source_key.display());
            } else {
                let output = Command::new("openssl")
                    .args(["rand", "-hex", "32"])
                    .output()
                    .context("Failed to run openssl rand")?;
                if !output.status.success() {
                    bail!("openssl rand failed");
                }
                let key_hex = String::from_utf8_lossy(&output.stdout);
                fs::write(&key_path, key_hex.trim())
                    .with_context(|| format!("write {}", key_path.display()))?;
                println!("Generated new secret key");
            }
            set_mode(&key_path, 0o600)?;
            set_owner(&key_path, SERVICE_USER, AGENTS_GROUP)?;
            run_install_cmd("chattr", &["+i", &key_path.to_string_lossy()])?;
        }
    }

    // ── Config symlink ──
    let symlink_path = home.join(".daemonclaw/config.toml");
    if !symlink_path.exists() && !symlink_path.is_symlink() {
        if dry_run {
            println!("[dry-run] would symlink config");
        } else {
            std::os::unix::fs::symlink(&etc_config, &symlink_path)
                .with_context(|| format!("symlink {}", symlink_path.display()))?;
        }
    }

    // ── systemd unit ──
    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let target_bin = Path::new("/usr/local/bin/daemonclaw");

    let podman_installed = Command::new("podman")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    let syscall_filter_line = if podman_installed {
        "SystemCallFilter=@system-service clone3 unshare setns pivot_root mount umount2\n"
    } else {
        ""
    };

    let unit = format!(
        r#"[Unit]
Description=DaemonClaw AI Agent
Documentation=https://github.com/DeliveryBoyTech/daemonclaw
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User={user}
Group={agents}
WorkingDirectory={home}

ExecStart={target_bin} daemon

Restart=on-failure
RestartSec=5
WatchdogSec=120

# Hardening
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ProtectKernelTunables=true
ProtectKernelModules=true
ProtectControlGroups=true
RestrictNamespaces=true
RestrictRealtime=true
RestrictSUIDSGID=true
LockPersonality=true
MemoryDenyWriteExecute=true
SystemCallArchitectures=native
{syscall_filter}
# Network: outbound + gateway port only
SocketBindAllow=tcp:42617
SocketBindDeny=any

# Paths
ReadWritePaths={home}
ReadOnlyPaths=/proc /sys /etc/os-release /etc/hostname /etc/resolv.conf {etc}

# Resource limits
MemoryMax=2G
CPUQuota=200%
TasksMax=64
LimitFSIZE=1073741824

# Environment
Environment=RUST_LOG=info
Environment=HOME={home}

# Logging
StandardOutput=journal
StandardError=journal
SyslogIdentifier=daemonclaw

[Install]
WantedBy=multi-user.target
"#,
        user = SERVICE_USER,
        agents = AGENTS_GROUP,
        home = HOME_DIR,
        etc = ETC_DIR,
        target_bin = target_bin.display(),
        syscall_filter = syscall_filter_line,
    );

    let unit_path = Path::new("/etc/systemd/system/daemonclaw.service");
    let timer_path = Path::new("/etc/systemd/system/daemonclaw-backup.timer");
    let backup_svc_path = Path::new("/etc/systemd/system/daemonclaw-backup.service");
    let tmpfiles_path = Path::new("/etc/tmpfiles.d/daemonclaw-backups.conf");

    if dry_run {
        println!("[dry-run] would write systemd units and backup infrastructure");
        println!("[dry-run] would copy binary to {}", target_bin.display());
        println!();
        println!("[dry-run] No changes were made.");
    } else {
        fs::write(unit_path, &unit)
            .with_context(|| format!("write {}", unit_path.display()))?;

        // Backup timer
        if !Path::new(BACKUP_DIR).exists() {
            fs::create_dir_all(BACKUP_DIR)
                .with_context(|| format!("mkdir {}", BACKUP_DIR))?;
            set_mode(Path::new(BACKUP_DIR), 0o750)?;
            set_owner(Path::new(BACKUP_DIR), "root", AGENTS_GROUP)?;
        }

        let timer = "[Unit]\nDescription=DaemonClaw state backup\n\n\
                     [Timer]\nOnCalendar=hourly\nPersistent=true\n\n\
                     [Install]\nWantedBy=timers.target\n";
        fs::write(timer_path, timer)
            .with_context(|| format!("write {}", timer_path.display()))?;

        let backup_svc = format!(
            "[Unit]\nDescription=DaemonClaw state backup\n\n\
             [Service]\nType=oneshot\n\
             ExecStart=/bin/bash -c 'ts=$(date +%%Y%%m%%d-%%H%%M%%S) && \
             tar czf {backup}/state-${{ts}}.tar.gz -C {home}/.daemonclaw state/'\n\
             User=root\nGroup={agents}\n",
            backup = BACKUP_DIR, home = HOME_DIR, agents = AGENTS_GROUP,
        );
        fs::write(backup_svc_path, &backup_svc)
            .with_context(|| format!("write {}", backup_svc_path.display()))?;

        let tmpfiles = format!(
            "d {backup} 0750 root {agents} - -\ne {backup} - - - 30d -\n",
            backup = BACKUP_DIR, agents = AGENTS_GROUP,
        );
        fs::write(tmpfiles_path, &tmpfiles)
            .with_context(|| format!("write {}", tmpfiles_path.display()))?;

        // Enable units
        run_install_cmd("systemctl", &["daemon-reload"])?;
        run_install_cmd("systemctl", &["enable", "daemonclaw.service"])?;
        run_install_cmd("systemctl", &["enable", "daemonclaw-backup.timer"])?;
        run_install_cmd("systemctl", &["start", "daemonclaw-backup.timer"])?;

        // Copy binary
        if exe.to_string_lossy() != target_bin.to_string_lossy() {
            fs::copy(&exe, target_bin)
                .with_context(|| format!("copy binary to {}", target_bin.display()))?;
            set_mode(target_bin, 0o755)?;
        }

        run_install_cmd("systemctl", &["start", "daemonclaw.service"])?;

        println!("Installed daemonclaw system service: {}", unit_path.display());
        println!("   Config: {}", etc_config.display());
        println!("   Status: systemctl status daemonclaw");
    }

    Ok(())
}

fn resolve_invoking_user_home() -> Option<PathBuf> {
    let sudo_user = std::env::var("SUDO_USER")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty() && v != "root");

    if let Some(user) = sudo_user {
        if let Ok(output) = Command::new("getent").args(["passwd", &user]).output() {
            if output.status.success() {
                let entry = String::from_utf8_lossy(&output.stdout);
                let fields: Vec<&str> = entry.trim().split(':').collect();
                if fields.len() >= 6 {
                    return Some(PathBuf::from(fields[5]));
                }
            }
        }
    }

    None
}

fn resolve_invoking_user_config() -> Option<PathBuf> {
    let home = resolve_invoking_user_home()?;
    let path = home.join(".daemonclaw/config.toml");
    if path.exists() { Some(path) } else { None }
}

fn resolve_invoking_user_secret_key() -> Option<PathBuf> {
    let home = resolve_invoking_user_home()?;
    let path = home.join(".daemonclaw/.secret_key");
    if path.exists() { Some(path) } else { None }
}

fn generate_install_config() -> String {
    r#"# DaemonClaw configuration — generated by daemonclaw service install
# See https://github.com/DeliveryBoyTech/daemonclaw for documentation.
schema_version = 2

[providers]
fallback = "zai"

[providers.models.zai]
# api_key = "your-api-key-here"
model = "glm-5-turbo"
temperature = 0.7
timeout_secs = 120

[observability]
backend = "none"
runtime_trace_mode = "full"
runtime_trace_path = "state/runtime-trace.jsonl"
runtime_trace_max_entries = 1000

[autonomy]
level = "supervised"
workspace_only = true
allowed_commands = [
    "git", "ls", "cat", "grep", "find", "echo", "pwd", "cd",
    "wc", "head", "tail", "date",
    "mkdir", "cp", "mv", "rm", "touch", "ln", "readlink", "realpath", "chmod",
    "sed", "awk", "diff", "patch", "sort", "uniq", "tr", "cut", "xargs",
    "tee", "less", "file", "stat", "basename", "dirname",
    "tar", "gzip", "gunzip", "zip", "unzip",
    "curl", "wget",
    "sh", "bash",
    "python", "python3", "pip", "pip3",
    "node", "npm", "npx", "yarn", "pnpm",
    "rustc", "rustup", "cargo",
    "go", "deno", "bun",
    "make", "cmake", "gcc", "cc", "g++", "ld", "pkg-config",
    "apt", "apt-get", "dpkg", "dpkg-query", "snap",
    "gh", "docker", "docker-compose", "podman",
    "lsb_release", "systemctl", "journalctl",
    "test", "true", "false", "which", "type", "env",
    "uname", "uptime", "free", "df", "du", "ps", "id", "whoami",
    "hostname", "nproc", "lscpu", "lsmem", "lsblk", "lsof",
    "ss", "netstat", "ip", "ping", "traceroute", "nslookup", "dig",
    "sudo",
]
forbidden_paths = [
    "/etc/shadow", "/etc/gshadow",
    "/etc/sudoers", "/etc/sudoers.d",
    "/etc/pam.d", "/etc/polkit-1",
    "/etc/ssh",
    "~/.ssh", "~/.gnupg", "~/.aws",
    "/root", "/home",
    "~/.daemonclaw/.secret_key",
    "~/.daemonclaw/workspace/memory",
    "~/.daemonclaw/workspace/sessions",
    "~/.daemonclaw/workspace/devices.db",
]
max_actions_per_hour = 240
max_cost_per_day_cents = 2500
require_approval_for_medium_risk = true
block_high_risk_commands = true
shell_env_passthrough = ["PATH", "HOME", "USER", "LANG", "LC_ALL", "TERM", "CARGO_HOME", "RUSTUP_HOME"]
auto_approve = [
    "file_read", "file_write", "file_edit",
    "memory_recall",
    "web_search_tool", "web_fetch",
    "calculator",
    "glob_search", "content_search",
    "image_info", "weather",
    "shell",
]
always_ask = []
allowed_roots = [
    "~/.cargo",
    "~/.rustup",
    "~/.npm",
    "~/.local",
    "/tmp",
    "/proc", "/sys",
    "/etc/os-release",
    "/etc/hostname",
    "~/.gitconfig",
    "~/.config/gh",
]
non_cli_excluded_tools = []

[security.sandbox]
backend = "none"
firejail_args = []

[security.resources]
max_memory_mb = 8192
max_cpu_time_seconds = 1800
max_subprocesses = 64
memory_monitoring = true

[security.audit]
enabled = true
log_path = "audit.log"
max_size_mb = 100
sign_events = false

[security.otp]
enabled = false

[security.estop]
enabled = false

[backup]
enabled = true
max_keep = 10
include_dirs = ["config", "memory", "audit", "knowledge", "skills"]
destination_dir = "state/backups"
compress = true
encrypt = false

[data_retention]
enabled = false
retention_days = 90

[runtime]
kind = "native"

[reliability]
provider_retries = 2
provider_backoff_ms = 500
fallback_providers = []

[scheduler]
enabled = true
max_tasks = 64
max_concurrent = 4

[agent]
compact_context = true
max_tool_iterations = 100
max_history_messages = 150
max_context_tokens = 200000
parallel_tools = false
tool_dispatcher = "auto"

[agent.context_compression]
enabled = false

[agent.thinking]
default_level = "medium"

[skills]
open_skills_enabled = true
allow_scripts = true

# [channels.telegram]
# enabled = true
# bot_token = "your-telegram-bot-token"
# allowed_users = []
"#.to_string()
}

/// Check if the current process is running as root (Unix only)
#[cfg(unix)]
fn is_root() -> bool {
    // SAFETY: `getuid()` is a simple system call that returns the real user ID of the calling
    // process. It is always safe to call as it takes no arguments and returns a scalar value.
    // This is a well-established pattern in Rust for getting the current user ID.
    unsafe { libc::getuid() == 0 }
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

/// Check if the daemonclaw user exists and has expected properties.
/// Returns Ok if user doesn't exist (OpenRC will handle creation or fail gracefully).
/// Returns error if user exists but has unexpected properties.
fn check_daemonclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "daemonclaw"]).output();
    let is_alpine = Path::new("/etc/alpine-release").exists();

    let (del_cmd, add_cmd) = if is_alpine {
        (
            "deluser daemonclaw && delgroup daemonclaw",
            "addgroup -S daemonclaw && adduser -S -s /sbin/nologin -H -D -G daemonclaw daemonclaw",
        )
    } else {
        ("userdel daemonclaw", "useradd -r -s /sbin/nologin daemonclaw")
    };

    match output {
        Ok(output) if output.status.success() => {
            let passwd_entry = String::from_utf8_lossy(&output.stdout);
            let parts: Vec<&str> = passwd_entry.split(':').collect();
            if parts.len() >= 7 {
                let uid = parts[2];
                let gid = parts[3];
                let home = parts[5];
                let shell = parts[6];

                if uid.parse::<u32>().unwrap_or(999) >= 1000 {
                    bail!(
                        "User 'daemonclaw' exists but has unexpected UID {} (expected system UID < 1000).\n\
                         Recreate with: sudo {} && sudo {}",
                        uid,
                        del_cmd,
                        add_cmd
                    );
                }

                if !shell.contains("nologin") && !shell.contains("false") {
                    bail!(
                        "User 'daemonclaw' exists but has unexpected shell '{}'.\n\
                         Expected nologin/false for security. Fix with: sudo {} && sudo {}",
                        shell,
                        del_cmd,
                        add_cmd
                    );
                }

                if home != "/var/lib/daemonclaw" && home != "/nonexistent" {
                    eprintln!(
                        "⚠️  Warning: daemonclaw user has home directory '{}' (expected /var/lib/daemonclaw or /nonexistent)",
                        home
                    );
                }

                let _ = gid;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

fn ensure_daemonclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "daemonclaw"]).output();
    if let Ok(output) = output
        && output.status.success()
    {
        return check_daemonclaw_user();
    }

    let is_alpine = Path::new("/etc/alpine-release").exists();

    if is_alpine {
        let group_output = Command::new("getent").args(["group", "daemonclaw"]).output();
        let group_exists = group_output.map(|o| o.status.success()).unwrap_or(false);

        if !group_exists {
            let output = Command::new("addgroup")
                .args(["-S", "daemonclaw"])
                .output()
                .context("Failed to create daemonclaw group")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Failed to create daemonclaw group: {}", stderr.trim());
            }
            println!("✅ Created system group: daemonclaw");
        }

        let output = Command::new("adduser")
            .args([
                "-S",
                "-s",
                "/sbin/nologin",
                "-H",
                "-D",
                "-G",
                "daemonclaw",
                "daemonclaw",
            ])
            .output()
            .context("Failed to create daemonclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create daemonclaw user: {}", stderr.trim());
        }
    } else {
        let output = Command::new("useradd")
            .args(["-r", "-s", "/sbin/nologin", "daemonclaw"])
            .output()
            .context("Failed to create daemonclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create daemonclaw user: {}", stderr.trim());
        }
    }

    println!("✅ Created system user: daemonclaw");
    Ok(())
}

/// Change ownership of a path to daemonclaw:daemonclaw
#[cfg(unix)]
fn chown_to_daemonclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["daemonclaw:daemonclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to change ownership of {} to daemonclaw:daemonclaw: {}",
            path.display(),
            stderr.trim(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_to_daemonclaw(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn chown_recursive_to_daemonclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["-R", "daemonclaw:daemonclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run recursive chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to recursively change ownership of {} to daemonclaw:daemonclaw: {}",
            path.display(),
            stderr.trim(),
        );
    }

    Ok(())
}

#[cfg(not(unix))]
fn chown_recursive_to_daemonclaw(_path: &Path) -> Result<()> {
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    fs::create_dir_all(target)
        .with_context(|| format!("Failed to create directory {}", target.display()))?;

    for entry in fs::read_dir(source)
        .with_context(|| format!("Failed to read directory {}", source.display()))?
    {
        let entry = entry?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let file_type = entry
            .file_type()
            .with_context(|| format!("Failed to inspect {}", source_path.display()))?;

        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if file_type.is_file() {
            if target_path.exists() {
                continue;
            }
            fs::copy(&source_path, &target_path).with_context(|| {
                format!(
                    "Failed to copy file {} -> {}",
                    source_path.display(),
                    target_path.display()
                )
            })?;
        }
    }

    Ok(())
}

fn resolve_invoking_user_config_dir() -> Option<PathBuf> {
    let sudo_user = std::env::var("SUDO_USER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty() && value != "root");

    if let Some(user) = sudo_user
        && let Ok(output) = Command::new("getent").args(["passwd", &user]).output()
        && output.status.success()
    {
        let entry = String::from_utf8_lossy(&output.stdout);
        let fields: Vec<&str> = entry.trim().split(':').collect();
        if fields.len() >= 6 {
            return Some(PathBuf::from(fields[5]).join(".daemonclaw"));
        }
    }

    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".daemonclaw"))
}

fn migrate_openrc_runtime_state_if_needed(config_dir: &Path) -> Result<()> {
    let target_config = config_dir.join("config.toml");
    if target_config.exists() {
        println!(
            "✅ Reusing existing OpenRC config at {}",
            target_config.display()
        );
        return Ok(());
    }

    let Some(source_dir) = resolve_invoking_user_config_dir() else {
        return Ok(());
    };

    let source_config = source_dir.join("config.toml");
    if !source_config.exists() {
        return Ok(());
    }

    copy_dir_recursive(&source_dir, config_dir)?;
    println!(
        "✅ Migrated runtime state from {} to {}",
        source_dir.display(),
        config_dir.display()
    );
    Ok(())
}

#[cfg(unix)]
fn shell_single_quote(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

#[cfg(unix)]
fn build_openrc_writability_probe_command(path: &Path, has_runuser: bool) -> (String, Vec<String>) {
    let probe = format!("test -w {}", shell_single_quote(&path.to_string_lossy()));
    if has_runuser {
        (
            "runuser".to_string(),
            vec![
                "-u".to_string(),
                "daemonclaw".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                probe,
            ],
        )
    } else {
        (
            "su".to_string(),
            vec![
                "-s".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                probe,
                "daemonclaw".to_string(),
            ],
        )
    }
}

#[cfg(unix)]
fn ensure_openrc_runtime_path_writable(path: &Path) -> Result<()> {
    let has_runuser = which::which("runuser").is_ok();
    let (program, args) = build_openrc_writability_probe_command(path, has_runuser);
    let output = Command::new(&program)
        .args(args.iter().map(String::as_str))
        .output()
        .with_context(|| {
            format!(
                "Failed to verify OpenRC runtime write access for {}",
                path.display()
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let details = if stderr.trim().is_empty() {
            "write-access probe failed"
        } else {
            stderr.trim()
        };
        bail!(
            "OpenRC runtime user 'daemonclaw' cannot write {} ({details}). \
             Re-run `sudo daemonclaw service install` and ensure ownership is daemonclaw:daemonclaw.",
            path.display(),
        );
    }

    Ok(())
}

#[cfg(unix)]
fn ensure_openrc_runtime_dirs_writable(
    config_dir: &Path,
    workspace_dir: &Path,
    log_dir: &Path,
) -> Result<()> {
    for path in [config_dir, workspace_dir, log_dir] {
        ensure_openrc_runtime_path_writable(path)?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_openrc_runtime_dirs_writable(
    _config_dir: &Path,
    _workspace_dir: &Path,
    _log_dir: &Path,
) -> Result<()> {
    Ok(())
}

/// Warn if the binary path is in a user home directory
fn warn_if_binary_in_home(exe_path: &Path) {
    let path_str = exe_path.to_string_lossy();
    if path_str.contains("/home/") || path_str.contains(".cargo/bin") {
        eprintln!(
            "⚠️  Warning: Binary path '{}' appears to be in a user home directory.\n\
             For system-wide OpenRC service, consider installing to /usr/local/bin:\n\
             sudo cp '{}' /usr/local/bin/daemonclaw",
            exe_path.display(),
            exe_path.display()
        );
    }
}

/// Generate OpenRC init script content (pure function for testability)
fn generate_openrc_script(exe_path: &Path, config_dir: &Path) -> String {
    format!(
        r#"#!/sbin/openrc-run

name="daemonclaw"
description="DaemonClaw daemon"

command="{exe}"
command_args="--config-dir {config_dir} daemon"
command_background="yes"
command_user="daemonclaw:daemonclaw"
pidfile="/run/${{RC_SVCNAME}}.pid"
umask 027
output_log="/var/log/daemonclaw/access.log"
error_log="/var/log/daemonclaw/error.log"

# Provide HOME so headless browsers can create profile/cache directories.
# Without this, Chromium/Firefox fail with sandbox or profile errors.
export HOME="/var/lib/daemonclaw"

depend() {{
    need net
    after firewall
}}

start_pre() {{
    checkpath --directory --owner daemonclaw:daemonclaw --mode 0750 /var/lib/daemonclaw
}}
"#,
        exe = exe_path.display(),
        config_dir = config_dir.display(),
    )
}

fn resolve_openrc_executable() -> Result<PathBuf> {
    let preferred = Path::new("/usr/local/bin/daemonclaw");
    if preferred.exists() {
        return Ok(preferred.to_path_buf());
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    Ok(exe)
}

fn install_linux_openrc(config: &Config) -> Result<()> {
    if !is_root() {
        bail!(
            "OpenRC service installation requires root privileges.\n\
             Please run with sudo: sudo daemonclaw service install"
        );
    }

    ensure_daemonclaw_user()?;

    let exe = resolve_openrc_executable()?;
    warn_if_binary_in_home(&exe);

    let config_dir = Path::new("/etc/daemonclaw");
    let workspace_dir = config_dir.join("workspace");
    let log_dir = Path::new("/var/log/daemonclaw");

    if !config_dir.exists() {
        fs::create_dir_all(config_dir)
            .with_context(|| format!("Failed to create {}", config_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(config_dir, fs::Permissions::from_mode(0o755)).with_context(
                || format!("Failed to set permissions on {}", config_dir.display()),
            )?;
        }
        println!("✅ Created directory: {}", config_dir.display());
    }

    migrate_openrc_runtime_state_if_needed(config_dir)?;

    if !workspace_dir.exists() {
        fs::create_dir_all(&workspace_dir)
            .with_context(|| format!("Failed to create {}", workspace_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o750)).with_context(
                || format!("Failed to set permissions on {}", workspace_dir.display()),
            )?;
        }
        chown_to_daemonclaw(&workspace_dir)?;
        println!(
            "✅ Created directory: {} (owned by daemonclaw:daemonclaw)",
            workspace_dir.display()
        );
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&workspace_dir, fs::Permissions::from_mode(0o750))
            .with_context(|| format!("Failed to set permissions on {}", workspace_dir.display()))?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(config_dir, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("Failed to set permissions on {}", config_dir.display()))?;
        let config_path = config_dir.join("config.toml");
        if config_path.exists() {
            fs::set_permissions(&config_path, fs::Permissions::from_mode(0o600)).with_context(
                || format!("Failed to set permissions on {}", config_path.display()),
            )?;
        }
        let secret_key_path = config_dir.join(".secret_key");
        if secret_key_path.exists() {
            fs::set_permissions(&secret_key_path, fs::Permissions::from_mode(0o600)).with_context(
                || format!("Failed to set permissions on {}", secret_key_path.display()),
            )?;
        }
    }

    chown_recursive_to_daemonclaw(config_dir)?;

    let created_log_dir = !log_dir.exists();
    if created_log_dir {
        fs::create_dir_all(log_dir)
            .with_context(|| format!("Failed to create {}", log_dir.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(log_dir, fs::Permissions::from_mode(0o750))
                .with_context(|| format!("Failed to set permissions on {}", log_dir.display()))?;
        }
    }

    chown_to_daemonclaw(log_dir)?;

    ensure_openrc_runtime_dirs_writable(config_dir, &workspace_dir, log_dir)?;

    if created_log_dir {
        println!(
            "✅ Created directory: {} (owned by daemonclaw:daemonclaw)",
            log_dir.display()
        );
    }

    let init_script = generate_openrc_script(&exe, config_dir);
    let init_path = Path::new("/etc/init.d/daemonclaw");
    fs::write(init_path, init_script)
        .with_context(|| format!("Failed to write {}", init_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(init_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("Failed to set permissions on {}", init_path.display()))?;
    }

    run_checked(Command::new("rc-update").args(["add", "daemonclaw", "default"]))?;
    println!("✅ Installed OpenRC service: /etc/init.d/daemonclaw");
    println!("   Config path: /etc/daemonclaw/config.toml");
    println!("   Start with: sudo daemonclaw service start");
    let _ = config;
    Ok(())
}

#[allow(dead_code)]
fn install_windows(config: &Config) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let logs_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("logs");
    fs::create_dir_all(&logs_dir)?;

    // Create a wrapper script that redirects output to log files
    let wrapper = logs_dir.join("daemonclaw-daemon.cmd");
    let stdout_log = logs_dir.join("daemon.stdout.log");
    let stderr_log = logs_dir.join("daemon.stderr.log");

    let wrapper_content = format!(
        "@echo off\r\n\"{}\" daemon >>\"{}\" 2>>\"{}\"",
        exe.display(),
        stdout_log.display(),
        stderr_log.display()
    );
    fs::write(&wrapper, &wrapper_content)?;

    let task_name = windows_task_name();

    // Remove any existing task first (ignore errors if it doesn't exist)
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", task_name, "/F"])
        .output();

    run_checked(Command::new("schtasks").args([
        "/Create",
        "/TN",
        task_name,
        "/SC",
        "ONLOGON",
        "/TR",
        &format!("\"{}\"", wrapper.display()),
        "/RL",
        "HIGHEST",
        "/F",
    ]))?;

    println!("✅ Installed Windows scheduled task: {}", task_name);
    println!("   Wrapper: {}", wrapper.display());
    println!("   Logs: {}", logs_dir.display());
    println!("   Start with: daemonclaw service start");
    Ok(())
}

fn macos_service_file() -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    Ok(home
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

#[allow(dead_code)]
fn linux_service_file(config: &Config) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    let _ = config;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join("daemonclaw.service"))
}

fn run_checked(command: &mut Command) -> Result<()> {
    let output = command.output().context("Failed to spawn command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Command failed: {}", stderr.trim());
    }
    Ok(())
}

pub fn run_capture(command: &mut Command) -> Result<String> {
    let output = command.output().context("Failed to spawn command")?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).to_string();
    }
    Ok(text)
}

pub fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(all(test, daemonclaw_root_crate))]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_escapes_reserved_chars() {
        let escaped = xml_escape("<&>\"' and text");
        assert_eq!(escaped, "&lt;&amp;&gt;&quot;&apos; and text");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_capture_reads_stdout() {
        let out = run_capture(Command::new("sh").args(["-c", "echo hello"]))
            .expect("stdout capture should succeed");
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_capture_falls_back_to_stderr() {
        let out = run_capture(Command::new("sh").args(["-c", "echo warn 1>&2"]))
            .expect("stderr capture should succeed");
        assert_eq!(out.trim(), "warn");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_checked_errors_on_non_zero_status() {
        let err = run_checked(Command::new("sh").args(["-c", "exit 17"]))
            .expect_err("non-zero exit should error");
        assert!(err.to_string().contains("Command failed"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn linux_service_file_has_expected_suffix() {
        let file = linux_service_file(&Config::default()).unwrap();
        let path = file.to_string_lossy();
        assert!(path.ends_with(".config/systemd/user/daemonclaw.service"));
    }

    #[test]
    fn windows_task_name_is_constant() {
        assert_eq!(windows_task_name(), "DaemonClaw Daemon");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_capture_reads_stdout_windows() {
        let out = run_capture(Command::new("cmd").args(["/C", "echo hello"]))
            .expect("stdout capture should succeed");
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn run_checked_errors_on_non_zero_status_windows() {
        let err = run_checked(Command::new("cmd").args(["/C", "exit /b 17"]))
            .expect_err("non-zero exit should error");
        assert!(err.to_string().contains("Command failed"));
    }

    #[test]
    fn init_system_from_str_parses_valid_values() {
        assert_eq!("auto".parse::<InitSystem>().unwrap(), InitSystem::Auto);
        assert_eq!("AUTO".parse::<InitSystem>().unwrap(), InitSystem::Auto);
        assert_eq!(
            "systemd".parse::<InitSystem>().unwrap(),
            InitSystem::Systemd
        );
        assert_eq!(
            "SYSTEMD".parse::<InitSystem>().unwrap(),
            InitSystem::Systemd
        );
        assert_eq!("openrc".parse::<InitSystem>().unwrap(), InitSystem::Openrc);
        assert_eq!("OPENRC".parse::<InitSystem>().unwrap(), InitSystem::Openrc);
    }

    #[test]
    fn init_system_from_str_rejects_unknown() {
        let err = "unknown"
            .parse::<InitSystem>()
            .expect_err("should reject unknown");
        assert!(err.to_string().contains("Unknown init system"));
        assert!(err.to_string().contains("Supported: auto, systemd, openrc"));
    }

    #[test]
    fn init_system_default_is_auto() {
        assert_eq!(InitSystem::default(), InitSystem::Auto);
    }

    #[cfg(unix)]
    #[test]
    fn is_root_matches_system_uid() {
        // SAFETY: `getuid()` is a simple system call that returns the real user ID of the calling
        // process. It is always safe to call as it takes no arguments and returns a scalar value.
        // This test verifies our `is_root()` wrapper returns the same result as the raw syscall.
        assert_eq!(is_root(), unsafe { libc::getuid() == 0 });
    }

    #[test]
    fn generate_openrc_script_contains_required_directives() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/daemonclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/daemonclaw"));

        assert!(script.starts_with("#!/sbin/openrc-run"));
        assert!(script.contains("name=\"daemonclaw\""));
        assert!(script.contains("description=\"DaemonClaw daemon\""));
        assert!(script.contains("command=\"/usr/local/bin/daemonclaw\""));
        assert!(script.contains("command_args=\"--config-dir /etc/daemonclaw daemon\""));
        assert!(!script.contains("env DAEMONCLAW_CONFIG_DIR"));
        assert!(!script.contains("env DAEMONCLAW_WORKSPACE"));
        assert!(script.contains("command_background=\"yes\""));
        assert!(script.contains("command_user=\"daemonclaw:daemonclaw\""));
        assert!(script.contains("pidfile=\"/run/${RC_SVCNAME}.pid\""));
        assert!(script.contains("umask 027"));
        assert!(script.contains("output_log=\"/var/log/daemonclaw/access.log\""));
        assert!(script.contains("error_log=\"/var/log/daemonclaw/error.log\""));
        assert!(script.contains("depend()"));
        assert!(script.contains("need net"));
        assert!(script.contains("after firewall"));
    }

    #[test]
    fn generate_openrc_script_sets_home_for_browser() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/daemonclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/daemonclaw"));

        assert!(
            script.contains("export HOME=\"/var/lib/daemonclaw\""),
            "OpenRC script must set HOME for headless browser support"
        );
    }

    #[test]
    fn generate_openrc_script_creates_home_directory() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/daemonclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/daemonclaw"));

        assert!(
            script.contains("start_pre()"),
            "OpenRC script must have start_pre to create HOME dir"
        );
        assert!(
            script.contains("checkpath --directory --owner daemonclaw:daemonclaw"),
            "start_pre must ensure /var/lib/daemonclaw exists with correct ownership"
        );
    }

    #[test]
    fn systemd_unit_contains_home_and_pass_environment() {
        let unit = "[Unit]\n\
             Description=DaemonClaw daemon\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             ExecStart=/usr/local/bin/daemonclaw daemon\n\
             Restart=always\n\
             RestartSec=3\n\
             # Ensure HOME is set so headless browsers can create profile/cache dirs.\n\
             Environment=HOME=%h\n\
             # Allow inheriting DISPLAY and XDG_RUNTIME_DIR from the user session\n\
             # so graphical/headless browsers can function correctly.\n\
             PassEnvironment=DISPLAY XDG_RUNTIME_DIR\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n"
            .to_string();

        assert!(
            unit.contains("Environment=HOME=%h"),
            "systemd unit must set HOME for headless browser support"
        );
        assert!(
            unit.contains("PassEnvironment=DISPLAY XDG_RUNTIME_DIR"),
            "systemd unit must pass through display/runtime env vars"
        );
    }

    #[test]
    fn warn_if_binary_in_home_detects_home_path() {
        use std::path::PathBuf;

        let home_path = PathBuf::from("/home/user/.cargo/bin/daemonclaw");
        assert!(home_path.to_string_lossy().contains("/home/"));
        assert!(home_path.to_string_lossy().contains(".cargo/bin"));

        let cargo_path = PathBuf::from("/home/user/.cargo/bin/daemonclaw");
        assert!(cargo_path.to_string_lossy().contains(".cargo/bin"));

        let system_path = PathBuf::from("/usr/local/bin/daemonclaw");
        assert!(!system_path.to_string_lossy().contains("/home/"));
        assert!(!system_path.to_string_lossy().contains(".cargo/bin"));
    }

    #[cfg(unix)]
    #[test]
    fn shell_single_quote_escapes_single_quotes() {
        assert_eq!(
            shell_single_quote("/tmp/weird'path"),
            "'/tmp/weird'\"'\"'path'"
        );
    }

    #[cfg(unix)]
    #[test]
    fn openrc_writability_probe_prefers_runuser_when_available() {
        let (program, args) =
            build_openrc_writability_probe_command(Path::new("/etc/daemonclaw"), true);
        assert_eq!(program, "runuser");
        assert_eq!(
            args,
            vec![
                "-u".to_string(),
                "daemonclaw".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/daemonclaw'".to_string()
            ]
        );
    }

    #[test]
    fn detect_homebrew_var_dir_from_cellar_path() {
        let exe = PathBuf::from("/opt/homebrew/Cellar/daemonclaw/1.2.3/bin/daemonclaw");
        let var_dir = detect_homebrew_var_dir(&exe);
        assert_eq!(var_dir, Some(PathBuf::from("/opt/homebrew/var/daemonclaw")));
    }

    #[test]
    fn detect_homebrew_var_dir_intel_cellar_path() {
        let exe = PathBuf::from("/usr/local/Cellar/daemonclaw/1.0.0/bin/daemonclaw");
        let var_dir = detect_homebrew_var_dir(&exe);
        assert_eq!(var_dir, Some(PathBuf::from("/usr/local/var/daemonclaw")));
    }

    #[test]
    fn detect_homebrew_var_dir_non_homebrew_path() {
        let exe = PathBuf::from("/home/user/.cargo/bin/daemonclaw");
        let var_dir = detect_homebrew_var_dir(&exe);
        assert_eq!(var_dir, None);
    }

    #[cfg(unix)]
    #[test]
    fn openrc_writability_probe_falls_back_to_su() {
        let (program, args) =
            build_openrc_writability_probe_command(Path::new("/etc/daemonclaw/workspace"), false);
        assert_eq!(program, "su");
        assert_eq!(
            args,
            vec![
                "-s".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/daemonclaw/workspace'".to_string(),
                "daemonclaw".to_string()
            ]
        );
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn tail_file_errors_on_missing_file() {
        let missing = Path::new("/tmp/daemonclaw-test-nonexistent-log-file.log");
        let result = tail_file(missing, 10, false);
        assert!(result.is_err(), "tail on missing file should fail");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn tail_file_reads_existing_file() {
        let dir = tempfile::tempdir().expect("failed to create temp dir");
        let log = dir.path().join("test-tail.log");
        fs::write(&log, "line1\nline2\nline3\nline4\nline5\n").unwrap();
        // tail should succeed on existing file
        let result = tail_file(&log, 3, false);
        assert!(result.is_ok(), "tail on existing file should succeed");
    }

    #[test]
    fn logs_variant_is_recognized() {
        // Ensure the Logs variant can be constructed and matched
        let cmd = crate::ServiceCommands::Logs {
            lines: 25,
            follow: true,
        };
        match &cmd {
            crate::ServiceCommands::Logs { lines, follow } => {
                assert_eq!(*lines, 25);
                assert!(*follow);
            }
            _ => panic!("Expected Logs variant"),
        }
    }
}
