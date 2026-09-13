//! Locate and launch a bounded desktop daemon supervisor when none is already running.

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper, JobObject, KillOnDrop};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::time::Instant;

const READINESS_FRAME_MAX_BYTES: usize = 4096;
const CAPABILITY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
const SIGTERM: i32 = 15;
#[cfg(unix)]
const SIGKILL: i32 = 9;
#[cfg(unix)]
const ESRCH: i32 = 3;
#[cfg(unix)]
const EPERM: i32 = 1;

#[cfg(unix)]
unsafe extern "C" {
    fn kill(pid: i32, signal: i32) -> i32;
}

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
    ensure_desktop_supervisor_capability(binary)?;
    let mut cmd = desktop_daemon_command(binary, port);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
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

    let mut child = cmd.spawn()?;
    let stdout = match child.stdout.take() {
        Some(stdout) => stdout,
        None => {
            let startup_error = std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "desktop supervisor stdout unavailable",
            );
            return Err(attach_cleanup_error(
                startup_error,
                terminate_supervisor_tree(&mut child),
            ));
        }
    };
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_readiness_frame(stdout));
    });
    let frame = match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => {
            let startup_error = std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "desktop supervisor readiness timed out",
            );
            return Err(attach_cleanup_error(
                startup_error,
                terminate_supervisor_tree(&mut child),
            ));
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            let startup_error = std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "desktop supervisor readiness reader exited unexpectedly",
            );
            return Err(attach_cleanup_error(
                startup_error,
                terminate_supervisor_tree(&mut child),
            ));
        }
    };
    match validate_readiness_frame(frame, || child.try_wait()) {
        Ok(()) => Ok(child),
        Err(startup_error) => Err(attach_cleanup_error(
            startup_error,
            terminate_supervisor_tree(&mut child),
        )),
    }
}

fn validate_readiness_frame<F>(
    frame: std::io::Result<Option<String>>,
    status_probe: F,
) -> std::io::Result<()>
where
    F: FnOnce() -> std::io::Result<Option<std::process::ExitStatus>>,
{
    let line = match frame {
        Ok(Some(line)) => line,
        Ok(None) => {
            let status = status_probe().map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "failed to inspect desktop supervisor after readiness pipe closed: {error}"
                    ),
                )
            })?;
            let detail = status
                .map(|status| format!("desktop supervisor exited before readiness ({status})"))
                .unwrap_or_else(|| "desktop supervisor closed readiness pipe".to_string());
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                detail,
            ));
        }
        Err(error) => return Err(error),
    };
    parse_readiness_line(&line).map_err(std::io::Error::other)
}

fn ensure_desktop_supervisor_capability(binary: &Path) -> std::io::Result<()> {
    ensure_desktop_supervisor_capability_with_timeout(binary, CAPABILITY_PROBE_TIMEOUT)
}

fn ensure_desktop_supervisor_capability_with_timeout(
    binary: &Path,
    timeout: Duration,
) -> std::io::Result<()> {
    let mut command = Command::new(binary);
    command
        .args(["service", "run-desktop-daemon", "--help"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    let mut child = {
        let mut command = CommandWrap::from(tokio::process::Command::from(command));
        command
            .wrap(KillOnDrop)
            .wrap(WindowsProbeSpawnFailureGuard)
            .wrap(JobObject);
        command.spawn().map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!(
                    "failed to check Desktop supervisor support in {}: {error}",
                    binary.display()
                ),
            )
        })?
    };
    #[cfg(not(windows))]
    let mut child = command.spawn().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "failed to check Desktop supervisor support in {}: {error}",
                binary.display()
            ),
        )
    })?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                return Err(attach_capability_cleanup_error(
                    error,
                    terminate_capability_probe(&mut child),
                ));
            }
        }
        if Instant::now() >= deadline {
            let timeout_error = std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                format!(
                    "timed out checking Desktop supervisor support in {}; install or bundle a ZeroClaw kernel that supports the Desktop supervisor command",
                    binary.display()
                ),
            );
            return Err(attach_capability_cleanup_error(
                timeout_error,
                terminate_capability_probe(&mut child),
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let cleanup_result = terminate_capability_probe(&mut child);
    if status.success() {
        return cleanup_result;
    }
    let unsupported_error = std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!(
            "the ZeroClaw kernel at {} does not support the required Desktop supervisor command; install or bundle a kernel that supports this command",
            binary.display()
        ),
    );
    Err(attach_capability_cleanup_error(
        unsupported_error,
        cleanup_result,
    ))
}

#[cfg(not(windows))]
fn terminate_capability_probe(child: &mut Child) -> std::io::Result<()> {
    terminate_supervisor_tree(child)
}

#[cfg(windows)]
fn terminate_capability_probe(child: &mut Box<dyn ChildWrapper>) -> std::io::Result<()> {
    child.start_kill()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out reaping capability probe",
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Reaps a still-suspended probe if Job Object setup fails after process creation.
#[cfg(windows)]
#[derive(Debug)]
struct WindowsProbeSpawnFailureGuard;

#[cfg(windows)]
impl CommandWrapper for WindowsProbeSpawnFailureGuard {
    fn wrap_child(
        &mut self,
        child: Box<dyn ChildWrapper>,
        _core: &CommandWrap,
    ) -> std::io::Result<Box<dyn ChildWrapper>> {
        Ok(Box::new(WindowsProbeSpawnFailureChild {
            child: Some(child),
        }))
    }
}

#[cfg(windows)]
#[derive(Debug)]
struct WindowsProbeSpawnFailureChild {
    child: Option<Box<dyn ChildWrapper>>,
}

#[cfg(windows)]
impl ChildWrapper for WindowsProbeSpawnFailureChild {
    fn inner(&self) -> &dyn ChildWrapper {
        self.child.as_deref().expect("guard child must be present")
    }

    fn inner_mut(&mut self) -> &mut dyn ChildWrapper {
        self.child
            .as_deref_mut()
            .expect("guard child must be present")
    }

    fn into_inner(mut self: Box<Self>) -> Box<dyn ChildWrapper> {
        self.child.take().expect("guard child must be present")
    }
}

#[cfg(windows)]
impl Drop for WindowsProbeSpawnFailureChild {
    fn drop(&mut self) {
        let Some(child) = self.child.as_deref_mut() else {
            return;
        };
        let _ = child.start_kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            match child.try_wait() {
                Ok(Some(_)) | Err(_) => break,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
    }
}

fn attach_capability_cleanup_error(
    probe_error: std::io::Error,
    cleanup_result: std::io::Result<()>,
) -> std::io::Error {
    match cleanup_result {
        Ok(()) => probe_error,
        Err(cleanup_error) => std::io::Error::new(
            probe_error.kind(),
            format!("{probe_error}; capability probe cleanup failed: {cleanup_error}"),
        ),
    }
}

fn read_readiness_frame<R: Read>(mut reader: R) -> std::io::Result<Option<String>> {
    let mut frame = Vec::with_capacity(READINESS_FRAME_MAX_BYTES + 1);
    let bytes_read = std::io::BufReader::new(&mut reader)
        .take((READINESS_FRAME_MAX_BYTES + 1) as u64)
        .read_until(b'\n', &mut frame)?;
    if bytes_read == 0 {
        return Ok(None);
    }
    if frame.len() > READINESS_FRAME_MAX_BYTES && frame.last().copied() != Some(b'\n')
        || frame.len() > READINESS_FRAME_MAX_BYTES + 1
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("desktop supervisor readiness exceeded {READINESS_FRAME_MAX_BYTES} bytes"),
        ));
    }
    if frame.last().copied() != Some(b'\n') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "desktop supervisor readiness ended before newline",
        ));
    }
    frame.pop();
    String::from_utf8(frame).map(Some).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "desktop supervisor readiness was not valid UTF-8",
        )
    })
}

fn attach_cleanup_error(
    startup_error: std::io::Error,
    cleanup_result: std::io::Result<()>,
) -> std::io::Error {
    match cleanup_result {
        Ok(()) => startup_error,
        Err(cleanup_error) => std::io::Error::new(
            startup_error.kind(),
            format!("{startup_error}; supervisor cleanup failed: {cleanup_error}"),
        ),
    }
}

fn terminate_supervisor_tree(child: &mut Child) -> std::io::Result<()> {
    let mut utility_errors = Vec::new();
    let mut forceful_termination_initiated = false;

    #[cfg(unix)]
    {
        let pid = child.id();
        // The supervisor is started in its own process group, so a negative
        // PID targets only that owned group and its descendant daemon.
        if let Err(error) = signal_supervisor_group(pid, SIGTERM) {
            utility_errors.push(error.to_string());
        } else {
            let deadline = Instant::now() + Duration::from_millis(250);
            while Instant::now() < deadline {
                if let Err(error) = child.try_wait() {
                    utility_errors.push(format!("failed to reap supervisor: {error}"));
                    break;
                }
                match supervisor_group_still_running(pid) {
                    Ok(false) => break,
                    Ok(true) => std::thread::sleep(Duration::from_millis(10)),
                    Err(error) => {
                        utility_errors.push(format!(
                            "failed to inspect supervisor process group: {error}"
                        ));
                        break;
                    }
                }
            }
        }
        match supervisor_group_still_running(pid) {
            Ok(true) => {
                if let Err(error) = signal_supervisor_group(pid, SIGKILL) {
                    utility_errors.push(error.to_string());
                } else {
                    forceful_termination_initiated = true;
                    let deadline = Instant::now() + Duration::from_millis(250);
                    while Instant::now() < deadline {
                        if let Err(error) = child.try_wait() {
                            utility_errors.push(format!("failed to reap supervisor: {error}"));
                            break;
                        }
                        match supervisor_group_still_running(pid) {
                            Ok(false) => break,
                            Ok(true) => std::thread::sleep(Duration::from_millis(10)),
                            Err(error) => {
                                utility_errors.push(format!(
                                    "failed to verify supervisor process group cleanup: {error}"
                                ));
                                break;
                            }
                        }
                    }
                }
            }
            Ok(false) => {}
            Err(error) => utility_errors.push(format!(
                "failed to inspect supervisor process group before escalation: {error}"
            )),
        }
    }
    #[cfg(windows)]
    {
        let pid = child.id().to_string();
        for force in [false, true] {
            let mut command = Command::new("taskkill");
            command.args(["/PID", &pid, "/T"]);
            if force {
                command.arg("/F");
            }
            match command.status() {
                Ok(status) if status.success() => {
                    if force {
                        forceful_termination_initiated = true;
                    }
                    if !force {
                        let deadline = Instant::now() + Duration::from_millis(250);
                        while Instant::now() < deadline {
                            match child.try_wait() {
                                Ok(Some(_)) => break,
                                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                                Err(error) => {
                                    utility_errors
                                        .push(format!("failed to inspect supervisor: {error}"));
                                    break;
                                }
                            }
                        }
                    }
                }
                Ok(status) => utility_errors.push(format!("taskkill exited with status {status}")),
                Err(error) => utility_errors.push(format!("taskkill failed: {error}")),
            }
            if !child_still_running(child) {
                break;
            }
        }
    }

    if child_still_running(child) {
        match child.kill() {
            Ok(()) => forceful_termination_initiated = true,
            Err(error) => utility_errors.push(format!("fallback child kill failed: {error}")),
        }
    }
    let child_running = child_still_running(child);
    if (!child_running || forceful_termination_initiated)
        && let Err(error) = child.wait()
    {
        utility_errors.push(format!("failed to reap supervisor: {error}"));
    }
    if child_still_running(child) {
        utility_errors.push("supervisor remained running after cleanup".to_string());
    }
    #[cfg(unix)]
    match supervisor_group_still_running(child.id()) {
        Ok(true) => {
            utility_errors.push("supervisor process group remained after cleanup".to_string())
        }
        Ok(false) => {}
        Err(error) => utility_errors.push(format!(
            "failed to verify supervisor process group cleanup: {error}"
        )),
    }
    if utility_errors.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(utility_errors.join("; ")))
    }
}

fn child_still_running(child: &mut Child) -> bool {
    !matches!(child.try_wait(), Ok(Some(_)))
}

#[cfg(unix)]
fn signal_supervisor_group(pid: u32, signal: i32) -> std::io::Result<()> {
    let pid = i32::try_from(pid)
        .map_err(|_| std::io::Error::other("supervisor PID does not fit in pid_t"))?;
    let result = unsafe { kill(-pid, signal) };
    let error = std::io::Error::last_os_error();
    if result == 0 || error.raw_os_error() == Some(ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn supervisor_group_still_running(pid: u32) -> std::io::Result<bool> {
    let pid = i32::try_from(pid)
        .map_err(|_| std::io::Error::other("supervisor PID does not fit in pid_t"))?;
    let result = unsafe { kill(-pid, 0) };
    let error = std::io::Error::last_os_error();
    if result == 0 || error.raw_os_error() == Some(EPERM) {
        Ok(true)
    } else if error.raw_os_error() == Some(ESRCH) {
        Ok(false)
    } else {
        Err(error)
    }
}

fn parse_readiness_line(line: &str) -> Result<(), String> {
    let line = line.trim_end();
    if line == "READY" {
        return Ok(());
    }
    if let Some(message) = line.strip_prefix("ERROR ")
        && !message.trim().is_empty()
    {
        return Err(message.trim().to_string());
    }
    if line.is_empty() {
        Err("desktop supervisor returned an empty readiness response".to_string())
    } else {
        Err(format!(
            "desktop supervisor returned an invalid readiness response: {line}"
        ))
    }
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
    #[cfg(unix)]
    use std::fs;

    #[test]
    fn desktop_command_targets_hidden_supervisor_and_port() {
        let command = desktop_daemon_command(Path::new("/tmp/zeroclaw"), 42617);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["service", "run-desktop-daemon", "--port", "42617"]);
    }

    #[cfg(unix)]
    #[test]
    fn unsupported_kernel_reports_selected_path_and_matching_version_action() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw-desktop-old-kernel-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let binary = dir.join("old-zeroclaw");
        let descendant_pid_file = dir.join("unsupported-child.pid");
        let descendant_pid_file_literal =
            descendant_pid_file.to_string_lossy().replace('\'', "'\\''");
        let fixture = format!(
            "#!/bin/sh\n\
             trap '' HUP TERM INT\n\
             sleep 30 &\n\
             printf '%s' \"$!\" > '{descendant_pid_file_literal}'\n\
             exit 64\n"
        );
        fs::write(&binary, fixture).expect("write old kernel fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make old kernel fixture executable");

        let error = spawn_daemon(&binary, 0).expect_err("old kernel must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains(&binary.display().to_string()));
        assert!(error.to_string().contains("supports this command"));
        assert!(!error.to_string().contains("cleanup failed"));
        let descendant_pid: i32 = fs::read_to_string(&descendant_pid_file)
            .expect("fixture should record unsupported probe descendant pid")
            .parse()
            .expect("unsupported probe descendant pid should be numeric");
        let result = unsafe { kill(descendant_pid, 0) };
        assert_eq!(result, -1, "unsupported probe descendant remained alive");
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(ESRCH));
        fs::remove_dir_all(&dir).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn capability_probe_times_out_and_reaps_stale_kernel() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw-desktop-stale-kernel-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let binary = dir.join("stale-zeroclaw");
        let fixture = "#!/bin/sh\n\
             trap '' HUP TERM INT\n\
             sleep 30 &\n\
             child=$!\n\
             wait \"$child\"\n";
        fs::write(&binary, fixture).expect("write stale kernel fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make stale kernel fixture executable");

        let error =
            ensure_desktop_supervisor_capability_with_timeout(&binary, Duration::from_millis(100))
                .expect_err("stale kernel capability probe must time out");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains(&binary.display().to_string()));
        assert!(!error.to_string().contains("cleanup failed"));

        fs::remove_dir_all(&dir).expect("remove fixture directory");
    }

    #[test]
    fn readiness_line_parser_accepts_ready() {
        assert_eq!(parse_readiness_line("READY\n"), Ok(()));
    }

    #[test]
    fn readiness_line_parser_surfaces_error_detail() {
        assert_eq!(
            parse_readiness_line("ERROR could not open desktop log\n"),
            Err("could not open desktop log".to_string())
        );
    }

    #[test]
    fn readiness_line_parser_rejects_invalid_and_empty_lines() {
        let invalid = parse_readiness_line("NOT_READY\n").expect_err("invalid line");
        assert!(invalid.contains("NOT_READY"));
        assert_eq!(
            parse_readiness_line("\n"),
            Err("desktop supervisor returned an empty readiness response".to_string())
        );
    }

    #[test]
    fn readiness_frame_rejects_oversized_and_unterminated_input() {
        let oversized = vec![b'x'; READINESS_FRAME_MAX_BYTES + 1];
        let error = read_readiness_frame(oversized.as_slice()).expect_err("oversized frame");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("exceeded"));

        let error = read_readiness_frame(b"READY".as_slice()).expect_err("unterminated frame");
        assert_eq!(error.kind(), std::io::ErrorKind::UnexpectedEof);
        assert!(error.to_string().contains("newline"));
    }

    #[test]
    fn readiness_frame_preserves_status_probe_error() {
        let error = validate_readiness_frame(Ok(None), || {
            Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "simulated status probe failure",
            ))
        })
        .expect_err("status-probe failure must reject readiness");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(
            error
                .to_string()
                .contains("failed to inspect desktop supervisor after readiness pipe closed")
        );
        assert!(error.to_string().contains("simulated status probe failure"));
    }

    #[test]
    fn cleanup_failure_is_attached_to_startup_error() {
        let startup = std::io::Error::new(std::io::ErrorKind::TimedOut, "readiness timed out");
        let cleanup = std::io::Error::other("process tree still running");
        let error = attach_cleanup_error(startup, Err(cleanup));
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(error.to_string().contains("readiness timed out"));
        assert!(error.to_string().contains("supervisor cleanup failed"));
        assert!(error.to_string().contains("process tree still running"));
    }

    #[cfg(unix)]
    #[test]
    fn spawn_daemon_cleans_supervisor_tree_after_log_open_failure() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw-desktop-log-open-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let pid_file = dir.join("descendant.pid");
        let supervisor_pid_file = dir.join("supervisor.pid");
        let log_destination = dir.join("zeroclaw-desktop-daemon.log");
        let binary = dir.join("desktop-supervisor-fixture");
        let pid_file_literal = pid_file.to_string_lossy().replace('\'', "'\\''");
        let supervisor_pid_file_literal =
            supervisor_pid_file.to_string_lossy().replace('\'', "'\\''");
        let log_destination_literal = log_destination.to_string_lossy().replace('\'', "'\\''");
        fs::create_dir(&log_destination).expect("make log destination a directory");
        let fixture = format!(
            "#!/bin/sh\n\
             if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
             sleep 30 &\n\
             child=$!\n\
             printf '%s' \"$$\" > '{supervisor_pid_file_literal}'\n\
             printf '%s' \"$child\" > '{pid_file_literal}'\n\
             if open_error=$(printf '%s' 'desktop bootstrap' 2>&1 >> '{log_destination_literal}'); then\n\
                 printf '%s\\n' 'ERROR desktop log open unexpectedly succeeded'\n\
             else\n\
                 printf '%s\\n' \"ERROR failed to open desktop log {log_destination_literal}: $open_error\"\n\
             fi\n\
             trap 'kill \"$child\" 2>/dev/null || true; wait \"$child\" 2>/dev/null || true; exit 0' TERM INT\n\
             while kill -0 \"$child\" 2>/dev/null; do sleep 1; done\n"
        );
        fs::write(&binary, fixture).expect("write supervisor fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make supervisor fixture executable");

        let error = spawn_daemon(&binary, 0).expect_err("log-open failure must reject startup");
        let error = error.to_string();
        let detail_prefix = format!("failed to open desktop log {}: ", log_destination.display());
        let (_, detail) = error
            .split_once(&detail_prefix)
            .expect("parent error should identify the failed log destination");
        assert!(
            !detail.trim().is_empty(),
            "log-open detail must not be empty"
        );

        let supervisor_pid: i32 = fs::read_to_string(&supervisor_pid_file)
            .expect("fixture should record supervisor pid")
            .parse()
            .expect("supervisor pid should be numeric");
        let descendant_pid: i32 = fs::read_to_string(&pid_file)
            .expect("fixture should record descendant pid")
            .parse()
            .expect("descendant pid should be numeric");
        for (label, pid) in [
            ("supervisor", supervisor_pid),
            ("descendant", descendant_pid),
        ] {
            let deadline = Instant::now() + Duration::from_secs(2);
            let mut exited = false;
            while Instant::now() < deadline {
                let result = unsafe { kill(pid, 0) };
                let errno = std::io::Error::last_os_error().raw_os_error();
                if result == -1 && errno == Some(ESRCH) {
                    exited = true;
                    break;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(exited, "{label} process {pid} remained alive after cleanup");
        }
        fs::remove_dir_all(&dir).expect("remove fixture directory");
    }

    #[cfg(unix)]
    #[test]
    fn spawn_daemon_kills_group_when_supervisor_exits_before_cleanup() {
        use std::os::unix::fs::PermissionsExt;

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "zeroclaw-desktop-exiting-supervisor-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir(&dir).expect("create fixture directory");
        let pid_file = dir.join("descendant.pid");
        let binary = dir.join("exiting-supervisor-fixture");
        let pid_file_literal = pid_file.to_string_lossy().replace('\'', "'\\''");
        let fixture = format!(
            "#!/bin/sh\n\
             if [ \"${{1:-}}\" = service ] && [ \"${{2:-}}\" = run-desktop-daemon ] && [ \"${{3:-}}\" = --help ]; then exit 0; fi\n\
             trap '' HUP TERM INT\n\
             sleep 30 &\n\
             child=$!\n\
             printf '%s' \"$child\" > '{pid_file_literal}'\n\
             printf '%s\\n' 'INVALID'\n\
             exit 0\n"
        );
        fs::write(&binary, fixture).expect("write exiting supervisor fixture");
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700))
            .expect("make exiting supervisor fixture executable");

        let error = spawn_daemon(&binary, 0).expect_err("invalid readiness must reject startup");
        assert!(error.to_string().contains("invalid readiness response"));
        assert!(!error.to_string().contains("cleanup failed"));
        let descendant_pid: i32 = fs::read_to_string(&pid_file)
            .expect("fixture should record descendant pid")
            .parse()
            .expect("descendant pid should be numeric");
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            let result = unsafe { kill(descendant_pid, 0) };
            if result == -1 && std::io::Error::last_os_error().raw_os_error() == Some(ESRCH) {
                fs::remove_dir_all(&dir).expect("remove fixture directory");
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("descendant process {descendant_pid} remained alive after cleanup");
    }
}
