//! Locate and launch a `zeroclaw daemon` when none is already running.
//!
//! The desktop app reuses an existing gateway/daemon on its port when one
//! answers; otherwise it spawns its own `zeroclaw daemon` (the supervisor
//! mode — `gateway start` alone can't hot-reload after the Quickstart).
//!
//! The spawned daemon is detached into its own process group so it keeps
//! running as a background service after the app quits; the next launch finds
//! it healthy and reuses it. It inherits the app's environment, so an operator
//! (or a test harness) can point it at a specific config via `ZEROCLAW_CONFIG_DIR`.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

/// Filename of the kernel binary on the current platform.
fn zeroclaw_exe_name() -> &'static str {
    if cfg!(windows) {
        "zeroclaw.exe"
    } else {
        "zeroclaw"
    }
}

/// Find the `zeroclaw` binary. Checks, in order: the directory next to this
/// app (installed side-by-side), every `PATH` entry, then the common install
/// locations a GUI launch's minimal `PATH` usually misses.
pub fn find_zeroclaw_binary() -> Option<PathBuf> {
    let exe_name = zeroclaw_exe_name();

    // 1. Sibling of the desktop executable.
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(exe_name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }

    // 2. Any directory on PATH.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    // 3. Common install locations (Finder/Dock launches inherit a minimal PATH
    //    that usually omits ~/.cargo/bin and the Homebrew prefixes).
    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        for rel in [".cargo/bin", ".local/bin"] {
            let candidate = home.join(rel).join(exe_name);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    for dir in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        let candidate = Path::new(dir).join(exe_name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    None
}

/// Spawn `zeroclaw daemon -p <port>`, detached so it outlives the app, with
/// stdio routed to a log file under the OS temp dir. The child handle is
/// returned but intentionally not reaped — the daemon is a background service.
pub fn spawn_daemon(binary: &Path, port: u16) -> std::io::Result<Child> {
    let log_path = std::env::temp_dir().join("zeroclaw-desktop-daemon.log");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let log_err = log.try_clone()?;

    let mut cmd = Command::new(binary);
    cmd.arg("daemon")
        .arg("-p")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err));

    // Detach so signals to the app's process group (e.g. Ctrl-C on a dev
    // `cargo run`) don't also stop the daemon, and so it survives app exit.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }

    cmd.spawn()
}
