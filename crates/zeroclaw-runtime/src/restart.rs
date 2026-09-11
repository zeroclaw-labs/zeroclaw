//! Post-update self-respawn for bare (unsupervised) processes.
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
use std::path::PathBuf;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};

static RESPAWN_REQUESTED: AtomicBool = AtomicBool::new(false);
static LAUNCH: OnceLock<LaunchCommand> = OnceLock::new();

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
}
