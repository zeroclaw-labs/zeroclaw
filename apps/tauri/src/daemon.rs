//! Locate and launch a bounded desktop daemon supervisor when none is already running.

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

/// Spawn the bounded desktop daemon supervisor, detached so it outlives the app.
/// The child handle is returned but intentionally not reaped because the
/// supervisor owns the daemon's background lifecycle and log capture.
pub fn spawn_daemon(binary: &Path, port: u16) -> std::io::Result<Child> {
    let mut cmd = desktop_daemon_command(binary, port);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // Detach so signals to the app's process group (e.g. Ctrl-C on a dev
    // `cargo run`) don't also stop the supervisor, and so it survives app exit.
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

fn desktop_daemon_command(binary: &Path, port: u16) -> Command {
    let mut cmd = Command::new(binary);
    cmd.arg("service")
        .arg("run-desktop-daemon")
        .arg("--port")
        .arg(port.to_string());
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_command_targets_hidden_supervisor_and_port() {
        let command = desktop_daemon_command(Path::new("/tmp/zeroclaw"), 42617);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["service", "run-desktop-daemon", "--port", "42617"]);
    }
}
