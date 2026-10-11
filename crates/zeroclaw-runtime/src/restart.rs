//! Post-update restart contracts for bare and desktop-supervised processes.
//!
//! When the dashboard applies an upgrade with auto-restart on a process that has
//! no supervisor (no systemd/launchd), the gateway calls [`request_respawn`] and
//! triggers the daemon's graceful shutdown (SIGTERM). After the daemon loop
//! tears down — which releases the listening port — `main` calls
//! [`respawn_if_requested`], which launches a detached child running the
//! freshly-swapped on-disk binary; the parent then exits.
//!
//! Doing the spawn *after* teardown (rather than from the gateway task) avoids a
//! port-bind race: by the time the child starts, the old listener is gone.
//!
//! The launch command (executable path + argv) is captured at startup via
//! [`record_launch`], *before* any binary swap: on Linux `current_exe()`
//! resolves to a `"…/zeroclaw (deleted)"` path once the running inode is
//! unlinked by the swap, which is not spawnable.

use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static RESPAWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static DESKTOP_RESTART_REQUESTED: AtomicBool = AtomicBool::new(false);
static LAUNCH: OnceLock<LaunchCommand> = OnceLock::new();

pub const DESKTOP_SUPERVISED_ENV: &str = "ZEROCLAW_DESKTOP_SUPERVISED";
pub const DESKTOP_RESTART_MARKER_ENV: &str = "ZEROCLAW_DESKTOP_RESTART_MARKER";
pub const DESKTOP_RESTART_EXIT_CODE: i32 = 75;

#[derive(Clone)]
struct LaunchCommand {
    exe: PathBuf,
    exe_resolved: bool,
    args: Vec<OsString>,
}

fn launch_command_from_resolved_exe(
    resolved_exe: Option<PathBuf>,
    args: Vec<OsString>,
) -> LaunchCommand {
    match resolved_exe {
        Some(exe) => LaunchCommand {
            exe,
            exe_resolved: true,
            args,
        },
        None => LaunchCommand {
            exe: PathBuf::from("zeroclaw"),
            exe_resolved: false,
            args,
        },
    }
}

fn remediation_executable(command: &LaunchCommand) -> Option<&std::path::Path> {
    command.exe_resolved.then_some(command.exe.as_path())
}

/// Capture the launch executable + args once, at startup (before any upgrade
/// swaps the binary). Idempotent — later calls are ignored.
pub fn record_launch() {
    let _ = LAUNCH.set(launch_command_from_resolved_exe(
        std::env::current_exe().ok(),
        std::env::args_os().skip(1).collect(),
    ));
}

/// Return the resolved launch executable when it is safe to use for remediation.
///
/// The respawn command may retain a bare `zeroclaw` fallback when executable
/// resolution failed, but that PATH-dependent fallback is intentionally excluded
/// here. A successfully resolved path remains stable across an in-app binary swap.
pub fn recorded_launch_executable() -> Option<&'static std::path::Path> {
    LAUNCH.get().and_then(remediation_executable)
}

/// Whether the daemon launch command has already been captured.
///
/// This distinguishes startup before capture from a captured command whose
/// executable could not be resolved.
pub fn launch_command_recorded() -> bool {
    LAUNCH.get().is_some()
}

/// Request a self-respawn after the daemon shuts down. The caller is expected to
/// also trigger the daemon's graceful shutdown so the loop tears down first.
pub fn request_respawn() {
    RESPAWN_REQUESTED.store(true, Ordering::SeqCst);
}

pub fn is_desktop_supervised() -> bool {
    std::env::var_os(DESKTOP_SUPERVISED_ENV).is_some_and(|value| value == "1")
}

pub fn request_desktop_restart() -> std::io::Result<()> {
    let marker = std::env::var_os(DESKTOP_RESTART_MARKER_ENV).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "desktop restart marker is not configured by the supervisor",
        )
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .mode(0o600)
            .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let marker = PathBuf::from(marker);
    let result = (|| {
        let mut file = options.open(&marker)?;
        file.write_all(b"zeroclaw-desktop-restart\n")?;
        file.sync_all()
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&marker);
        return Err(error);
    }
    DESKTOP_RESTART_REQUESTED.store(true, Ordering::SeqCst);
    Ok(())
}

pub fn desktop_restart_requested() -> bool {
    DESKTOP_RESTART_REQUESTED.load(Ordering::SeqCst)
}

/// Whether a self-respawn was requested.
pub fn respawn_requested() -> bool {
    RESPAWN_REQUESTED.load(Ordering::SeqCst)
}

/// In-process graceful-shutdown trigger. On unix the gateway self-signals
/// SIGTERM; on platforms without that (Windows), it fires this instead, and the
/// daemon's `wait_for_exit_signal` selects on [`shutdown_notify`] to return
/// `Shutdown`.
fn shutdown_cell() -> &'static tokio::sync::Notify {
    static SHUTDOWN: OnceLock<tokio::sync::Notify> = OnceLock::new();
    SHUTDOWN.get_or_init(tokio::sync::Notify::new)
}

/// Request a graceful daemon shutdown in-process (cross-platform). Pairs with a
/// prior [`request_respawn`] to turn the shutdown into a self-restart.
pub fn request_shutdown() {
    shutdown_cell().notify_one();
}

/// The global in-process shutdown trigger, for the daemon loop to await.
pub fn shutdown_notify() -> &'static tokio::sync::Notify {
    shutdown_cell()
}

/// If a respawn was requested, launch a detached child running the captured
/// launch command (now the new on-disk binary), and return its PID.
///
/// Call this *after* the daemon has torn down, so the listening port is free.
/// The child is detached (new session on unix; `DETACHED_PROCESS` on windows)
/// and inherits this process's stdio, so it logs wherever the daemon did. A bare
/// process has no supervisor,
/// so if the child fails to start the service stays down until restarted by
/// hand (the previous binary remains as a `.bak`).
pub fn respawn_if_requested() -> Option<u32> {
    if !respawn_requested() {
        return None;
    }
    let cmd = LAUNCH.get()?.clone();
    let mut command = std::process::Command::new(&cmd.exe);
    command.args(&cmd.args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // SAFETY: `pre_exec` runs in the forked child before exec; `setsid` is
        // async-signal-safe and only detaches us into a new session/process
        // group so the child outlives this process and any terminal hangup.
        unsafe {
            command.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS: no inherited console, so the child survives the
        // launching console closing. CREATE_NEW_PROCESS_GROUP: a Ctrl+C/Break to
        // the old group doesn't reach it. (Inherited file-handle stdio — e.g.
        // the daemon wrapper's log redirects — stays valid.)
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
    }

    match command.spawn() {
        Ok(child) => {
            let pid = child.id();
            ::zeroclaw_log::record!(
                INFO,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_attrs(::serde_json::json!({ "pid": pid })),
                "post-upgrade self-respawn launched"
            );
            Some(pid)
        }
        Err(e) => {
            ::zeroclaw_log::record!(
                WARN,
                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                    .with_outcome(::zeroclaw_log::EventOutcome::Unknown)
                    .with_attrs(::serde_json::json!({ "error": format!("{e}") })),
                "post-upgrade self-respawn failed; service will stay down until restarted"
            );
            None
        }
    }
}

/// Build-time marker `scripts/desktop/prepare-kernel.sh` sets when it builds
/// the kernel a ZeroClaw Desktop package ships as its sidecar. A kernel built
/// any other way (Homebrew, cargo, `install.sh`, the release archives, a
/// workspace build) does not carry it.
pub const DESKTOP_SIDECAR_BUILD_ENV: &str = "ZEROCLAW_DESKTOP_SIDECAR";

/// Whether this kernel was built as a desktop package's sidecar.
pub fn desktop_sidecar_build() -> bool {
    option_env!("ZEROCLAW_DESKTOP_SIDECAR") == Some("1")
}

/// The ZeroClaw Desktop app executable, which every desktop package installs
/// in the same directory as its kernel sidecar.
const DESKTOP_APP_EXECUTABLE: &str = if cfg!(windows) {
    "zeroclaw-desktop.exe"
} else {
    "zeroclaw-desktop"
};

/// Whether the kernel at `exe` still sits in its desktop package: the
/// directory the kernel really lives in (symlinks to it resolved) holds the
/// desktop app executable as a regular file. A symlink named like the app,
/// such as a launcher in a shared `bin` directory, does not count.
fn in_desktop_package_layout(exe: &std::path::Path) -> bool {
    let Ok(kernel) = std::fs::canonicalize(exe) else {
        return false;
    };
    kernel.parent().is_some_and(|dir| {
        std::fs::symlink_metadata(dir.join(DESKTOP_APP_EXECUTABLE))
            .is_ok_and(|entry| entry.file_type().is_file())
    })
}

/// Whether a kernel at `exe`, built with or without the sidecar marker,
/// belongs to a desktop package. Both are required: the marker proves the
/// binary was built as a sidecar, and the layout proves it has not been
/// copied out of its package.
fn desktop_package_owns(exe: &std::path::Path, sidecar_build: bool) -> bool {
    sidecar_build && in_desktop_package_layout(exe)
}

/// Whether the running kernel is a desktop package's sidecar. The desktop app
/// installer owns such a kernel's upgrades: swapping it in place would leave
/// the app and its kernel on different versions (and, on macOS, modify a
/// signed bundle). A kernel installed separately keeps its own upgrade path
/// even when the desktop app launches and supervises it. Cached: neither the
/// build nor the executable's location changes for the process lifetime.
pub fn desktop_bundled_kernel() -> bool {
    static CACHE: OnceLock<bool> = OnceLock::new();
    *CACHE.get_or_init(|| {
        recorded_launch_executable()
            .map(std::path::Path::to_path_buf)
            .or_else(|| std::env::current_exe().ok())
            .is_some_and(|exe| desktop_package_owns(&exe, desktop_sidecar_build()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unresolved_launch_keeps_respawn_fallback_but_not_remediation_path() {
        let command = launch_command_from_resolved_exe(None, Vec::new());

        assert_eq!(command.exe, PathBuf::from("zeroclaw"));
        assert!(
            remediation_executable(&command).is_none(),
            "bare PATH-dependent fallback must not be used for remediation"
        );
    }

    #[test]
    fn resolved_launch_is_available_for_remediation() {
        let path = PathBuf::from("/resolved/zeroclaw");
        let command = launch_command_from_resolved_exe(Some(path.clone()), Vec::new());

        assert_eq!(command.exe, path);
        assert_eq!(
            remediation_executable(&command),
            Some(command.exe.as_path())
        );
    }

    #[test]
    fn respawn_flag_defaults_false_until_requested() {
        // Note: process-global; this test owns the flag in a fresh test binary.
        assert!(!respawn_requested());
        request_respawn();
        assert!(respawn_requested());
    }

    /// An install directory holding a kernel and, when `app` is set, the
    /// desktop app executable beside it as a regular file.
    fn install_dir(app: bool) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("create install dir");
        let kernel = dir.path().join(if cfg!(windows) {
            "zeroclaw.exe"
        } else {
            "zeroclaw"
        });
        std::fs::write(&kernel, b"kernel").expect("write kernel");
        if app {
            std::fs::write(dir.path().join(DESKTOP_APP_EXECUTABLE), b"app")
                .expect("write desktop app");
        }
        (dir, kernel)
    }

    #[test]
    fn sidecar_beside_its_desktop_app_is_package_owned() {
        let (_dir, kernel) = install_dir(true);
        assert!(desktop_package_owns(&kernel, true));
    }

    #[test]
    fn kernel_not_built_as_a_sidecar_is_never_package_owned() {
        // A workspace build leaves `zeroclaw` and `zeroclaw-desktop` side by
        // side too; without the marker that is not a package.
        let (_dir, kernel) = install_dir(true);
        assert!(!desktop_package_owns(&kernel, false));
    }

    #[test]
    fn sidecar_copied_out_of_its_package_is_not_package_owned() {
        let (_dir, kernel) = install_dir(false);
        assert!(!desktop_package_owns(&kernel, true));
    }

    #[test]
    fn desktop_app_name_must_be_a_regular_file() {
        let (dir, kernel) = install_dir(false);
        std::fs::create_dir(dir.path().join(DESKTOP_APP_EXECUTABLE))
            .expect("create look-alike directory");
        assert!(!desktop_package_owns(&kernel, true));
    }

    /// The independently installed PATH kernel sits in a shared prefix next
    /// to a `zeroclaw-desktop` launcher symlink that points at a desktop app
    /// installed elsewhere, and the desktop app supervises it. It is not
    /// package-owned: not without the sidecar marker it lacks, and not even
    /// with it, because a launcher symlink is not the packaged app.
    #[cfg(unix)]
    #[test]
    fn path_kernel_beside_a_desktop_launcher_symlink_is_not_package_owned() {
        let (app_bundle, _) = install_dir(true);
        let app_executable = app_bundle.path().join(DESKTOP_APP_EXECUTABLE);
        let (prefix_bin, path_kernel) = install_dir(false);
        std::os::unix::fs::symlink(
            &app_executable,
            prefix_bin.path().join(DESKTOP_APP_EXECUTABLE),
        )
        .expect("add the launcher symlink");

        assert!(!desktop_package_owns(&path_kernel, false));
        assert!(!desktop_package_owns(&path_kernel, true));
    }

    #[cfg(unix)]
    #[test]
    fn path_symlink_to_a_packaged_sidecar_is_package_owned() {
        let (_bundle, sidecar) = install_dir(true);
        let bin = tempfile::tempdir().expect("create bin dir");
        let link = bin.path().join("zeroclaw");
        std::os::unix::fs::symlink(&sidecar, &link).expect("link the sidecar onto PATH");
        assert!(desktop_package_owns(&link, true));
    }

    #[test]
    fn test_builds_are_not_desktop_sidecars() {
        assert!(!desktop_sidecar_build());
        assert!(!desktop_bundled_kernel());
    }

    #[test]
    fn the_desktop_packaging_step_builds_sidecars_with_the_marker() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let prepare = std::fs::read_to_string(root.join("scripts/desktop/prepare-kernel.sh"))
            .expect("desktop kernel preparation script should be readable");
        let builds: Vec<&str> = prepare
            .lines()
            .filter(|line| line.contains("cargo build"))
            .filter(|line| !line.trim_start().starts_with('#') && !line.contains("echo"))
            .collect();
        assert!(
            !builds.is_empty(),
            "prepare-kernel.sh must build the sidecar"
        );
        for build in builds {
            assert!(
                build.contains(&format!("{DESKTOP_SIDECAR_BUILD_ENV}=1 cargo build")),
                "every sidecar build in prepare-kernel.sh must set {DESKTOP_SIDECAR_BUILD_ENV}=1: {build}"
            );
        }
    }

    #[test]
    fn desktop_app_executable_matches_the_tauri_package() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let manifest = std::fs::read_to_string(root.join("apps/tauri/Cargo.toml"))
            .expect("desktop app manifest should be readable");
        assert!(
            manifest.contains("name = \"zeroclaw-desktop\""),
            "desktop packages install the app as `zeroclaw-desktop`; update DESKTOP_APP_EXECUTABLE if the package is renamed"
        );
        let config = std::fs::read_to_string(root.join("apps/tauri/tauri.conf.json"))
            .expect("desktop app config should be readable");
        assert!(
            !config.contains("mainBinaryName"),
            "a Tauri `mainBinaryName` renames the installed app executable; update DESKTOP_APP_EXECUTABLE to match"
        );
    }
}
