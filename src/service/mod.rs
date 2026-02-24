use crate::config::Config;
use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;

const SERVICE_LABEL: &str = "com.zeroclaw.daemon";
const WINDOWS_TASK_NAME: &str = "ZeroClaw Daemon";

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

pub fn handle_command(
    command: &crate::ServiceCommands,
    config: &Config,
    init_system: InitSystem,
) -> Result<()> {
    match command {
        crate::ServiceCommands::Install => install(config, init_system),
        crate::ServiceCommands::Start => start(config, init_system),
        crate::ServiceCommands::Stop => stop(config, init_system),
        crate::ServiceCommands::Restart => restart(config, init_system),
        crate::ServiceCommands::Status => status(config, init_system),
        crate::ServiceCommands::Uninstall => uninstall(config, init_system),
    }
}

fn install(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
        install_macos(config)
    } else if cfg!(target_os = "linux") {
        let resolved = init_system.resolve()?;
        install_linux(config, resolved)
    } else if cfg!(target_os = "windows") {
        install_windows(config)
    } else {
        anyhow::bail!("Service management is supported on macOS and Linux only");
    }
}

fn start(config: &Config, init_system: InitSystem) -> Result<()> {
    if cfg!(target_os = "macos") {
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
            run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run_checked(Command::new("systemctl").args(["--user", "start", "zeroclaw.service"]))?;
        }
        InitSystem::Openrc => {
            run_checked(Command::new("rc-service").args(["zeroclaw", "start"]))?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service started");
    Ok(())
}

fn stop(config: &Config, init_system: InitSystem) -> Result<()> {
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
                run_checked(Command::new("systemctl").args(["--user", "stop", "zeroclaw.service"]));
        }
        InitSystem::Openrc => {
            let _ = run_checked(Command::new("rc-service").args(["zeroclaw", "stop"]));
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service stopped");
    Ok(())
}

fn restart(config: &Config, init_system: InitSystem) -> Result<()> {
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
            run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]))?;
            run_checked(Command::new("systemctl").args(["--user", "restart", "zeroclaw.service"]))?;
        }
        InitSystem::Openrc => {
            run_checked(Command::new("rc-service").args(["zeroclaw", "restart"]))?;
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    println!("✅ Service restarted");
    Ok(())
}

fn status(config: &Config, init_system: InitSystem) -> Result<()> {
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

fn status_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let out = run_capture(Command::new("systemctl").args([
                "--user",
                "is-active",
                "zeroclaw.service",
            ]))
            .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!("Unit: {}", linux_service_file(config)?.display());
        }
        InitSystem::Openrc => {
            let out = run_capture(Command::new("rc-service").args(["zeroclaw", "status"]))
                .unwrap_or_else(|_| "unknown".into());
            println!("Service state: {}", out.trim());
            println!("Unit: /etc/init.d/zeroclaw");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

fn uninstall(config: &Config, init_system: InitSystem) -> Result<()> {
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
            .join("zeroclaw-daemon.cmd");
        if wrapper.exists() {
            fs::remove_file(&wrapper).ok();
        }
        println!("✅ Service uninstalled");
        return Ok(());
    }

    anyhow::bail!("Service management is supported on macOS and Linux only")
}

fn uninstall_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => {
            let file = linux_service_file(config)?;
            if file.exists() {
                fs::remove_file(&file)
                    .with_context(|| format!("Failed to remove {}", file.display()))?;
            }
            let _ = run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]));
            println!("✅ Service uninstalled ({})", file.display());
        }
        InitSystem::Openrc => {
            let init_script = Path::new("/etc/init.d/zeroclaw");
            if init_script.exists() {
                if let Err(err) =
                    run_checked(Command::new("rc-update").args(["del", "zeroclaw", "default"]))
                {
                    eprintln!(
                        "⚠️  Warning: Could not remove zeroclaw from OpenRC default runlevel: {err}"
                    );
                }
                fs::remove_file(init_script)
                    .with_context(|| format!("Failed to remove {}", init_script.display()))?;
            }
            println!("✅ Service uninstalled (/etc/init.d/zeroclaw)");
        }
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
    Ok(())
}

fn install_macos(config: &Config) -> Result<()> {
    let file = macos_service_file()?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let logs_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("logs");
    fs::create_dir_all(&logs_dir)?;

    let stdout = logs_dir.join("daemon.stdout.log");
    let stderr = logs_dir.join("daemon.stderr.log");

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
  <key>StandardOutPath</key>
  <string>{stdout}</string>
  <key>StandardErrorPath</key>
  <string>{stderr}</string>
</dict>
</plist>
"#,
        label = SERVICE_LABEL,
        exe = xml_escape(&exe.display().to_string()),
        stdout = xml_escape(&stdout.display().to_string()),
        stderr = xml_escape(&stderr.display().to_string())
    );

    fs::write(&file, plist)?;
    println!("✅ Installed launchd service: {}", file.display());
    println!("   Start with: zeroclaw service start");
    Ok(())
}

fn install_linux(config: &Config, init_system: InitSystem) -> Result<()> {
    match init_system {
        InitSystem::Systemd => install_linux_systemd(config),
        InitSystem::Openrc => install_linux_openrc(config),
        InitSystem::Auto => unreachable!("Auto should be resolved before this point"),
    }
}

fn install_linux_systemd(config: &Config) -> Result<()> {
    let file = linux_service_file(config)?;
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }

    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let unit = format!(
        "[Unit]\nDescription=ZeroClaw daemon\nAfter=network.target\n\n[Service]\nType=simple\nExecStart={} daemon\nRestart=always\nRestartSec=3\n\n[Install]\nWantedBy=default.target\n",
        exe.display()
    );

    fs::write(&file, unit)?;
    let _ = run_checked(Command::new("systemctl").args(["--user", "daemon-reload"]));
    let _ = run_checked(Command::new("systemctl").args(["--user", "enable", "zeroclaw.service"]));
    println!("✅ Installed systemd user service: {}", file.display());
    println!("   Start with: zeroclaw service start");
    Ok(())
}

/// Check if the current process is running as root (Unix only)
#[cfg(unix)]
fn is_root() -> bool {
    current_uid() == Some(0)
}

#[cfg(not(unix))]
fn is_root() -> bool {
    false
}

#[cfg(unix)]
fn current_uid() -> Option<u32> {
    let output = Command::new("id").arg("-u").output().ok()?;
    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

/// Check if the zeroclaw user exists and has expected properties.
/// Returns Ok if user doesn't exist (OpenRC will handle creation or fail gracefully).
/// Returns error if user exists but has unexpected properties.
fn check_zeroclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "zeroclaw"]).output();
    let is_alpine = Path::new("/etc/alpine-release").exists();

    let (del_cmd, add_cmd) = if is_alpine {
        (
            "deluser zeroclaw && delgroup zeroclaw",
            "addgroup -S zeroclaw && adduser -S -s /sbin/nologin -H -D -G zeroclaw zeroclaw",
        )
    } else {
        ("userdel zeroclaw", "useradd -r -s /sbin/nologin zeroclaw")
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
                        "User 'zeroclaw' exists but has unexpected UID {} (expected system UID < 1000).\n\
                         Recreate with: sudo {} && sudo {}",
                        uid, del_cmd, add_cmd
                    );
                }

                if !shell.contains("nologin") && !shell.contains("false") {
                    bail!(
                        "User 'zeroclaw' exists but has unexpected shell '{}'.\n\
                         Expected nologin/false for security. Fix with: sudo {} && sudo {}",
                        shell,
                        del_cmd,
                        add_cmd
                    );
                }

                if home != "/var/lib/zeroclaw" && home != "/nonexistent" {
                    eprintln!(
                        "⚠️  Warning: zeroclaw user has home directory '{}' (expected /var/lib/zeroclaw or /nonexistent)",
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

fn ensure_zeroclaw_user() -> Result<()> {
    let output = Command::new("getent").args(["passwd", "zeroclaw"]).output();
    if let Ok(output) = output {
        if output.status.success() {
            return check_zeroclaw_user();
        }
    }

    let is_alpine = Path::new("/etc/alpine-release").exists();

    if is_alpine {
        let group_output = Command::new("getent").args(["group", "zeroclaw"]).output();
        let group_exists = group_output.map(|o| o.status.success()).unwrap_or(false);

        if !group_exists {
            let output = Command::new("addgroup")
                .args(["-S", "zeroclaw"])
                .output()
                .context("Failed to create zeroclaw group")?;

            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr);
                bail!("Failed to create zeroclaw group: {}", stderr.trim());
            }
            println!("✅ Created system group: zeroclaw");
        }

        let output = Command::new("adduser")
            .args([
                "-S",
                "-s",
                "/sbin/nologin",
                "-H",
                "-D",
                "-G",
                "zeroclaw",
                "zeroclaw",
            ])
            .output()
            .context("Failed to create zeroclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create zeroclaw user: {}", stderr.trim());
        }
    } else {
        let output = Command::new("useradd")
            .args(["-r", "-s", "/sbin/nologin", "zeroclaw"])
            .output()
            .context("Failed to create zeroclaw user")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!("Failed to create zeroclaw user: {}", stderr.trim());
        }
    }

    println!("✅ Created system user: zeroclaw");
    Ok(())
}

/// Change ownership of a path to zeroclaw:zeroclaw
#[cfg(unix)]
fn chown_to_zeroclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["zeroclaw:zeroclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to change ownership of {} to zeroclaw:zeroclaw: {}",
            path.display(),
            stderr.trim(),
        );
    }
    Ok(())
}

#[cfg(not(unix))]
fn chown_to_zeroclaw(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn chown_recursive_to_zeroclaw(path: &Path) -> Result<()> {
    let output = Command::new("chown")
        .args(["-R", "zeroclaw:zeroclaw", &path.to_string_lossy()])
        .output()
        .context("Failed to run recursive chown")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!(
            "Failed to recursively change ownership of {} to zeroclaw:zeroclaw: {}",
            path.display(),
            stderr.trim(),
        );
    }

    Ok(())
}

#[cfg(not(unix))]
fn chown_recursive_to_zeroclaw(_path: &Path) -> Result<()> {
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

    if let Some(user) = sudo_user {
        if let Ok(output) = Command::new("getent").args(["passwd", &user]).output() {
            if output.status.success() {
                let entry = String::from_utf8_lossy(&output.stdout);
                let fields: Vec<&str> = entry.trim().split(':').collect();
                if fields.len() >= 6 {
                    return Some(PathBuf::from(fields[5]).join(".zeroclaw"));
                }
            }
        }
    }

    std::env::var("HOME")
        .ok()
        .map(PathBuf::from)
        .map(|home| home.join(".zeroclaw"))
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
                "zeroclaw".to_string(),
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
                "zeroclaw".to_string(),
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
            "OpenRC runtime user 'zeroclaw' cannot write {} ({details}). \
             Re-run `sudo zeroclaw service install` and ensure ownership is zeroclaw:zeroclaw.",
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
             sudo cp '{}' /usr/local/bin/zeroclaw",
            exe_path.display(),
            exe_path.display()
        );
    }
}

/// Generate OpenRC init script content (pure function for testability)
fn generate_openrc_script(exe_path: &Path, config_dir: &Path) -> String {
    format!(
        r#"#!/sbin/openrc-run

name="zeroclaw"
description="ZeroClaw daemon"

command="{}"
command_args="--config-dir {} daemon"
command_background="yes"
command_user="zeroclaw:zeroclaw"
pidfile="/run/${{RC_SVCNAME}}.pid"
umask 027
output_log="/var/log/zeroclaw/access.log"
error_log="/var/log/zeroclaw/error.log"

depend() {{
    need net
    after firewall
}}
"#,
        exe_path.display(),
        config_dir.display()
    )
}

fn resolve_openrc_executable() -> Result<PathBuf> {
    let preferred = Path::new("/usr/local/bin/zeroclaw");
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
             Please run with sudo: sudo zeroclaw service install"
        );
    }

    ensure_zeroclaw_user()?;

    let exe = resolve_openrc_executable()?;
    warn_if_binary_in_home(&exe);

    let config_dir = Path::new("/etc/zeroclaw");
    let workspace_dir = config_dir.join("workspace");
    let log_dir = Path::new("/var/log/zeroclaw");

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
        chown_to_zeroclaw(&workspace_dir)?;
        println!(
            "✅ Created directory: {} (owned by zeroclaw:zeroclaw)",
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

    chown_recursive_to_zeroclaw(config_dir)?;

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

    chown_to_zeroclaw(log_dir)?;

    ensure_openrc_runtime_dirs_writable(config_dir, &workspace_dir, log_dir)?;

    if created_log_dir {
        println!(
            "✅ Created directory: {} (owned by zeroclaw:zeroclaw)",
            log_dir.display()
        );
    }

    let init_script = generate_openrc_script(&exe, config_dir);
    let init_path = Path::new("/etc/init.d/zeroclaw");
    fs::write(init_path, init_script)
        .with_context(|| format!("Failed to write {}", init_path.display()))?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(init_path, fs::Permissions::from_mode(0o755))
            .with_context(|| format!("Failed to set permissions on {}", init_path.display()))?;
    }

    run_checked(Command::new("rc-update").args(["add", "zeroclaw", "default"]))?;
    println!("✅ Installed OpenRC service: /etc/init.d/zeroclaw");
    println!("   Config path: /etc/zeroclaw/config.toml");
    println!("   Start with: sudo zeroclaw service start");
    let _ = config;
    Ok(())
}

fn install_windows(config: &Config) -> Result<()> {
    let exe = std::env::current_exe().context("Failed to resolve current executable")?;
    let logs_dir = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("logs");
    fs::create_dir_all(&logs_dir)?;

    // Create a wrapper script that redirects output to log files
    let wrapper = logs_dir.join("zeroclaw-daemon.cmd");
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
    println!("   Start with: zeroclaw service start");
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

fn linux_service_file(config: &Config) -> Result<PathBuf> {
    let home = directories::UserDirs::new()
        .map(|u| u.home_dir().to_path_buf())
        .context("Could not find home directory")?;
    let _ = config;
    Ok(home
        .join(".config")
        .join("systemd")
        .join("user")
        .join("zeroclaw.service"))
}

fn run_checked(command: &mut Command) -> Result<()> {
    let output = command.output().context("Failed to spawn command")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("Command failed: {}", stderr.trim());
    }
    Ok(())
}

fn run_capture(command: &mut Command) -> Result<String> {
    let output = command.output().context("Failed to spawn command")?;
    let mut text = String::from_utf8_lossy(&output.stdout).to_string();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stderr).to_string();
    }
    Ok(text)
}

fn xml_escape(raw: &str) -> String {
    raw.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
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
        let out = run_capture(Command::new("sh").args(["-lc", "echo hello"]))
            .expect("stdout capture should succeed");
        assert_eq!(out.trim(), "hello");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_capture_falls_back_to_stderr() {
        let out = run_capture(Command::new("sh").args(["-lc", "echo warn 1>&2"]))
            .expect("stderr capture should succeed");
        assert_eq!(out.trim(), "warn");
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn run_checked_errors_on_non_zero_status() {
        let err = run_checked(Command::new("sh").args(["-lc", "exit 17"]))
            .expect_err("non-zero exit should error");
        assert!(err.to_string().contains("Command failed"));
    }

    #[cfg(not(target_os = "windows"))]
    #[test]
    fn linux_service_file_has_expected_suffix() {
        let file = linux_service_file(&Config::default()).unwrap();
        let path = file.to_string_lossy();
        assert!(path.ends_with(".config/systemd/user/zeroclaw.service"));
    }

    #[test]
    fn windows_task_name_is_constant() {
        assert_eq!(windows_task_name(), "ZeroClaw Daemon");
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
        assert_eq!(is_root(), current_uid() == Some(0));
    }

    #[test]
    fn generate_openrc_script_contains_required_directives() {
        use std::path::PathBuf;

        let exe_path = PathBuf::from("/usr/local/bin/zeroclaw");
        let script = generate_openrc_script(&exe_path, Path::new("/etc/zeroclaw"));

        assert!(script.starts_with("#!/sbin/openrc-run"));
        assert!(script.contains("name=\"zeroclaw\""));
        assert!(script.contains("description=\"ZeroClaw daemon\""));
        assert!(script.contains("command=\"/usr/local/bin/zeroclaw\""));
        assert!(script.contains("command_args=\"--config-dir /etc/zeroclaw daemon\""));
        assert!(!script.contains("env ZEROCLAW_CONFIG_DIR"));
        assert!(!script.contains("env ZEROCLAW_WORKSPACE"));
        assert!(script.contains("command_background=\"yes\""));
        assert!(script.contains("command_user=\"zeroclaw:zeroclaw\""));
        assert!(script.contains("pidfile=\"/run/${RC_SVCNAME}.pid\""));
        assert!(script.contains("umask 027"));
        assert!(script.contains("output_log=\"/var/log/zeroclaw/access.log\""));
        assert!(script.contains("error_log=\"/var/log/zeroclaw/error.log\""));
        assert!(script.contains("depend()"));
        assert!(script.contains("need net"));
        assert!(script.contains("after firewall"));
    }

    #[test]
    fn warn_if_binary_in_home_detects_home_path() {
        use std::path::PathBuf;

        let home_path = PathBuf::from("/home/user/.cargo/bin/zeroclaw");
        assert!(home_path.to_string_lossy().contains("/home/"));
        assert!(home_path.to_string_lossy().contains(".cargo/bin"));

        let cargo_path = PathBuf::from("/home/user/.cargo/bin/zeroclaw");
        assert!(cargo_path.to_string_lossy().contains(".cargo/bin"));

        let system_path = PathBuf::from("/usr/local/bin/zeroclaw");
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
            build_openrc_writability_probe_command(Path::new("/etc/zeroclaw"), true);
        assert_eq!(program, "runuser");
        assert_eq!(
            args,
            vec![
                "-u".to_string(),
                "zeroclaw".to_string(),
                "--".to_string(),
                "sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/zeroclaw'".to_string()
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn openrc_writability_probe_falls_back_to_su() {
        let (program, args) =
            build_openrc_writability_probe_command(Path::new("/etc/zeroclaw/workspace"), false);
        assert_eq!(program, "su");
        assert_eq!(
            args,
            vec![
                "-s".to_string(),
                "/bin/sh".to_string(),
                "-c".to_string(),
                "test -w '/etc/zeroclaw/workspace'".to_string(),
                "zeroclaw".to_string()
            ]
        );
    }
}
