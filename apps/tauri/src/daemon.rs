//! Locate and launch a bounded desktop daemon supervisor when none is already running.

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, CommandWrapper, JobObject, KillOnDrop};
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Child;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
#[cfg(any(unix, windows))]
use std::time::Instant;

// Keep this wire-protocol limit aligned with DESKTOP_READINESS_FRAME_MAX_BYTES
// in zeroclaw-runtime's service module.
const READINESS_FRAME_MAX_BYTES: usize = 4096;
const CAPABILITY_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a supervisor that failed startup gets to exit before it is forced.
pub(crate) const STARTUP_CLEANUP_GRACE: Duration = Duration::from_millis(250);
/// How long to wait for a terminated job's supervisor to report its exit.
#[cfg(windows)]
const WINDOWS_JOB_EXIT_WAIT: Duration = Duration::from_secs(5);

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

/// The handle of a launched desktop supervisor, and the only thing that makes
/// the tree under it owned. On Unix it is the supervisor's `Child`, which
/// leads its own process group. On Windows it also holds the Job Object the
/// supervisor was created in, suspended, before it could start a descendant;
/// the tree is stopped through that job, never by process ID.
#[cfg(not(windows))]
pub type Supervisor = Child;
#[cfg(windows)]
pub type Supervisor = Box<dyn process_wrap::std::ChildWrapper>;

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

/// Spawn the bounded desktop daemon supervisor and take its readiness pipe.
/// The returned handle is the only proof that this app instance launched the
/// tree: the caller must hand it to its owner (see [`crate::ownership`]) before
/// waiting for readiness, so Quit can stop the tree at any point. The
/// supervisor owns the daemon's lifecycle and log capture.
pub(crate) fn spawn_supervisor(
    binary: &Path,
    port: u16,
) -> std::io::Result<(Supervisor, std::process::ChildStdout)> {
    let mut cmd = desktop_daemon_command(binary, port);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());

    // Its own process group, so signals aimed at the app's group (e.g. Ctrl-C
    // on a dev `cargo run`) don't reach it and cleanup can signal exactly this
    // tree. It keeps running when windows close or the app crashes; only an
    // app exit through the retained handle stops it.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(not(windows))]
    let mut child: Supervisor = cmd.spawn()?;
    // Created suspended and assigned to a job this handle owns before it
    // runs, so every descendant starts inside the job. The job does not kill
    // on close: an app crash leaves the tree running, as on Unix. If the job
    // cannot be set up after the process exists, the guard kills it.
    #[cfg(windows)]
    let mut child: Supervisor = {
        use process_wrap::std::{CommandWrap as StdCommandWrap, CreationFlags, JobObject};
        use windows::Win32::System::Threading::{
            CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW, DETACHED_PROCESS,
        };
        let guard = SetupFailureGuard::armed();
        let mut wrapped = StdCommandWrap::from(cmd);
        wrapped
            .wrap(CreationFlags(
                DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW,
            ))
            .wrap(guard.clone())
            .wrap(JobObject);
        let child = wrapped.spawn()?;
        guard.disarm();
        child
    };
    #[cfg(not(windows))]
    let stdout = child.stdout.take();
    #[cfg(windows)]
    let stdout = child.stdout().take();
    match stdout {
        Some(stdout) => Ok((child, stdout)),
        None => {
            let startup_error = std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "desktop supervisor stdout unavailable",
            );
            Err(attach_cleanup_error(
                startup_error,
                terminate_supervisor_tree(&mut child, STARTUP_CLEANUP_GRACE),
            ))
        }
    }
}

/// Wait for the supervisor's one readiness frame.
pub(crate) fn read_readiness(stdout: std::process::ChildStdout) -> std::io::Result<Option<String>> {
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(read_readiness_frame(stdout));
    });
    match receiver.recv_timeout(Duration::from_secs(10)) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "desktop supervisor readiness timed out",
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "desktop supervisor readiness reader exited unexpectedly",
        )),
    }
}

/// Kills and reaps a spawned process if the rest of its setup fails after the
/// process exists: on Windows, creating its job, assigning the suspended
/// process to it, or resuming it. Disarmed once the whole spawn succeeds, so a
/// supervisor that started keeps the job's no-kill-on-close crash policy.
#[cfg(any(windows, test))]
#[derive(Clone, Debug)]
struct SetupFailureGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);

#[cfg(any(windows, test))]
impl SetupFailureGuard {
    fn armed() -> Self {
        Self(std::sync::Arc::new(std::sync::atomic::AtomicBool::new(
            true,
        )))
    }

    fn disarm(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(any(windows, test))]
impl process_wrap::std::CommandWrapper for SetupFailureGuard {
    fn wrap_child(
        &mut self,
        child: Box<dyn process_wrap::std::ChildWrapper>,
        _core: &process_wrap::std::CommandWrap,
    ) -> std::io::Result<Box<dyn process_wrap::std::ChildWrapper>> {
        Ok(Box::new(SetupFailureChild {
            child: Some(child),
            armed: std::sync::Arc::clone(&self.0),
        }))
    }
}

#[cfg(any(windows, test))]
#[derive(Debug)]
struct SetupFailureChild {
    child: Option<Box<dyn process_wrap::std::ChildWrapper>>,
    armed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[cfg(any(windows, test))]
impl process_wrap::std::ChildWrapper for SetupFailureChild {
    fn inner(&self) -> &dyn process_wrap::std::ChildWrapper {
        self.child.as_deref().expect("guard child must be present")
    }

    fn inner_mut(&mut self) -> &mut dyn process_wrap::std::ChildWrapper {
        self.child
            .as_deref_mut()
            .expect("guard child must be present")
    }

    fn into_inner(mut self: Box<Self>) -> Box<dyn process_wrap::std::ChildWrapper> {
        self.child.take().expect("guard child must be present")
    }
}

#[cfg(any(windows, test))]
impl Drop for SetupFailureChild {
    fn drop(&mut self) {
        if !self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
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

pub(crate) fn validate_readiness_frame<F>(
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

pub(crate) fn ensure_desktop_supervisor_capability(binary: &Path) -> std::io::Result<()> {
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
        #[cfg(not(windows))]
        let polled = peek_supervisor_exit(&mut child);
        #[cfg(windows)]
        let polled = child.try_wait();
        match polled {
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
    terminate_supervisor_tree(child, STARTUP_CLEANUP_GRACE)
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
    if frame.len() > READINESS_FRAME_MAX_BYTES {
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

pub(crate) fn attach_cleanup_error(
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

/// Stop the process tree a supervisor handle leads, allowing `grace` for the
/// supervisor to stop its daemon before the tree is forced down.
///
/// Only the handle is trusted, never a PID looked up later. On Unix the
/// supervisor leads its own process group, and it is not reaped until every
/// group signal has been sent: while it is unreaped its PID, and so the group
/// ID, cannot be reused, so the signals reach only the tree this handle
/// launched. On Windows the open process handle keeps the PID from being
/// reused while it is signalled.
pub(crate) fn terminate_supervisor_tree(
    child: &mut Supervisor,
    grace: Duration,
) -> std::io::Result<()> {
    let mut utility_errors = Vec::new();

    #[cfg(unix)]
    {
        let pid = child.id();
        // ECHILD means the supervisor was already reaped, so its PID and group
        // ID are no longer reserved and must not be signalled.
        let reserved = !matches!(
            supervisor_exit_status(pid),
            Err(ref error) if error.raw_os_error() == Some(libc::ECHILD)
        );
        if reserved {
            terminate_reserved_group(child, pid, grace, &mut utility_errors);
        } else {
            utility_errors.push("supervisor was reaped before cleanup".to_string());
        }
        // Only checks from here on: once the group empties its ID may be
        // reused, so nothing is signalled after the reap.
        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            match supervisor_group_still_running(pid) {
                Ok(false) => break,
                Ok(true) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(true) => {
                    utility_errors
                        .push("supervisor process group remained after cleanup".to_string());
                    break;
                }
                Err(error) => {
                    utility_errors.push(format!(
                        "failed to verify supervisor process group cleanup: {error}"
                    ));
                    break;
                }
            }
        }
    }
    #[cfg(windows)]
    {
        // Terminating the job reaches exactly the processes created in it; no
        // process ID or parent relationship is consulted. There is no graceful
        // phase on Windows: the windowless supervisor has no console to signal.
        if let Err(error) = child.start_kill() {
            utility_errors.push(format!("failed to terminate the supervisor's job: {error}"));
        }
        let deadline = Instant::now() + grace.max(WINDOWS_JOB_EXIT_WAIT);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Ok(None) => {
                    utility_errors.push(
                        "supervisor remained running after its job was terminated".to_string(),
                    );
                    break;
                }
                Err(error) => {
                    utility_errors.push(format!("failed to inspect supervisor: {error}"));
                    break;
                }
            }
        }
    }

    if utility_errors.is_empty() {
        Ok(())
    } else {
        Err(std::io::Error::other(utility_errors.join("; ")))
    }
}

/// Signal and reap a supervisor that has not been reaped yet, so its PID and
/// process group are still reserved: SIGTERM to the group, up to `grace` for
/// the supervisor to exit, SIGKILL to whatever is left of the group, then the
/// reap.
#[cfg(unix)]
fn terminate_reserved_group(
    child: &mut Child,
    pid: u32,
    grace: Duration,
    utility_errors: &mut Vec<String>,
) {
    match signal_reserved_group(pid, SIGTERM) {
        Ok(()) => {
            let deadline = Instant::now() + grace;
            loop {
                match supervisor_exited(pid) {
                    Ok(true) => break,
                    Ok(false) if Instant::now() < deadline => {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Ok(false) => break,
                    Err(error) => {
                        utility_errors.push(format!("failed to inspect supervisor: {error}"));
                        break;
                    }
                }
            }
        }
        Err(error) => utility_errors.push(error.to_string()),
    }
    // Descendants that ignored SIGTERM or outlived the supervisor are still in
    // its group, which the unreaped supervisor keeps reserved.
    if let Err(error) = signal_reserved_group(pid, SIGKILL) {
        utility_errors.push(error.to_string());
        if let Err(error) = child.kill() {
            utility_errors.push(format!("fallback child kill failed: {error}"));
        }
    }
    if let Err(error) = child.wait() {
        utility_errors.push(format!("failed to reap supervisor: {error}"));
    }
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

/// Signal a process group whose leader has not been reaped. macOS refuses to
/// signal a group whose only member is its exited (zombie) leader with EPERM;
/// there is nothing left to signal then, so that case is not an error.
#[cfg(unix)]
fn signal_reserved_group(pid: u32, signal: i32) -> std::io::Result<()> {
    match signal_supervisor_group(pid, signal) {
        Err(error) if error.raw_os_error() == Some(EPERM) && supervisor_exited(pid)? => Ok(()),
        result => result,
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

/// The supervisor's exit status once it has exited, without reaping it:
/// `WNOWAIT` leaves it a zombie, so its PID and process group stay reserved
/// until cleanup reaps it.
#[cfg(unix)]
fn supervisor_exit_status(pid: u32) -> std::io::Result<Option<std::process::ExitStatus>> {
    use std::os::unix::process::ExitStatusExt;

    let id = libc::id_t::try_from(pid)
        .map_err(|_| std::io::Error::other("supervisor PID does not fit in id_t"))?;
    // SAFETY: an all-zero `siginfo_t` is a valid value.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is a valid, writable `siginfo_t` for the whole call.
    let result = unsafe {
        libc::waitid(
            libc::P_PID,
            id,
            &mut info,
            libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
        )
    };
    if result == -1 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: `waitid` succeeded, so `info` holds a child-status record (all
    // zero when the child has not exited yet).
    let (exited_pid, status) = unsafe { (info.si_pid(), info.si_status()) };
    if exited_pid == 0 {
        return Ok(None);
    }
    let raw = match info.si_code {
        libc::CLD_EXITED => (status & 0xff) << 8,
        libc::CLD_KILLED => status & 0x7f,
        libc::CLD_DUMPED => (status & 0x7f) | 0x80,
        code => {
            return Err(std::io::Error::other(format!(
                "unexpected supervisor state code {code}"
            )));
        }
    };
    Ok(Some(std::process::ExitStatus::from_raw(raw)))
}

/// Whether the supervisor has exited, without reaping it on Unix.
#[cfg(unix)]
pub(crate) fn supervisor_exited(pid: u32) -> std::io::Result<bool> {
    supervisor_exit_status(pid).map(|status| status.is_some())
}

/// The supervisor's exit status once it has exited. On Unix it is left
/// unreaped so cleanup can still signal its process group safely; on Windows
/// the open process handle already keeps its PID from being reused.
pub(crate) fn peek_supervisor_exit(
    child: &mut Supervisor,
) -> std::io::Result<Option<std::process::ExitStatus>> {
    #[cfg(unix)]
    {
        supervisor_exit_status(child.id())
    }
    #[cfg(not(unix))]
    {
        child.try_wait()
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

/// Mint a pairing code for this app through the kernel CLI.
///
/// The gateway's pairing-code admin routes accept only callers presenting its
/// owner-only admin token, because a loopback connection alone can be a
/// same-host proxy relaying a remote caller. The CLI runs as this user with
/// the same config, so it can read the token; the app asks it rather than
/// calling the route directly. Blocking: run it off the async runtime.
pub fn mint_pairing_code(binary: &Path, port: u16) -> Result<String, String> {
    let output = paircode_command(binary, port)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .map_err(|e| format!("could not run {}: {e}", binary.display()))?;
    if !output.status.success() {
        return Err(format!("get-paircode exited with {}", output.status));
    }
    parse_paircode_output(&output.stdout)
}

fn paircode_command(binary: &Path, port: u16) -> Command {
    let mut cmd = Command::new(binary);
    cmd.args(["gateway", "get-paircode", "--new", "--json", "--port"])
        .arg(port.to_string());
    cmd
}

/// Read the code from `get-paircode --json` output: the last line that is a
/// JSON object carrying a string `pairing_code`.
fn parse_paircode_output(stdout: &[u8]) -> Result<String, String> {
    String::from_utf8_lossy(stdout)
        .lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok())
        .find_map(|value| value["pairing_code"].as_str().map(String::from))
        .ok_or_else(|| "get-paircode returned no pairing code".to_string())
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

    /// Run a fixture's capability probe once, untimed. The first exec of a
    /// new file can take seconds on a loaded macOS host, which the launch's
    /// timed probe would otherwise be measuring.
    #[cfg(unix)]
    fn warm_probe(binary: &Path) {
        let status = Command::new(binary)
            .args(["service", "run-desktop-daemon", "--help"])
            .status()
            .expect("warm fixture");
        assert!(status.success(), "fixture must answer the capability probe");
    }

    /// Launch `binary` the way the app does, into a fresh owned registry.
    #[cfg(unix)]
    fn launch(binary: &Path) -> std::io::Result<()> {
        crate::ownership::OwnedProcesses::default().launch(binary, 0)
    }
    #[cfg(unix)]
    use std::fs;

    #[test]
    fn paircode_command_mints_through_the_cli_as_json() {
        let command = paircode_command(Path::new("/tmp/zeroclaw"), 42617);
        let args: Vec<_> = command
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            args,
            [
                "gateway",
                "get-paircode",
                "--new",
                "--json",
                "--port",
                "42617"
            ]
        );
    }

    #[test]
    fn paircode_output_yields_the_code_and_rejects_its_absence() {
        assert_eq!(
            parse_paircode_output(b"{\"pairing_code\":\"ABC123\",\"message\":null}\n"),
            Ok("ABC123".to_string())
        );
        assert_eq!(
            parse_paircode_output(b"noise\n{\"pairing_code\":\"XYZ\"}\n"),
            Ok("XYZ".to_string())
        );
        assert!(parse_paircode_output(b"{\"pairing_code\":null,\"message\":\"off\"}\n").is_err());
        assert!(parse_paircode_output(b"").is_err());
    }

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

        let error = launch(&binary).expect_err("old kernel must be rejected");
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
        let mut maximum = vec![b'x'; READINESS_FRAME_MAX_BYTES - 1];
        maximum.push(b'\n');
        assert_eq!(
            read_readiness_frame(maximum.as_slice()).expect("maximum frame"),
            Some("x".repeat(READINESS_FRAME_MAX_BYTES - 1))
        );

        let mut oversized = vec![b'x'; READINESS_FRAME_MAX_BYTES];
        oversized.push(b'\n');
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
        warm_probe(&binary);

        let error = launch(&binary).expect_err("log-open failure must reject startup");
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
        warm_probe(&binary);

        let error = launch(&binary).expect_err("invalid readiness must reject startup");
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

    /// A wrapper that fails after the process exists, as job creation,
    /// assignment or resumption can on Windows, and reports the PID it saw.
    #[cfg(unix)]
    #[derive(Debug)]
    struct FailAfterSpawn(std::sync::Arc<std::sync::Mutex<Option<u32>>>);

    #[cfg(unix)]
    impl process_wrap::std::CommandWrapper for FailAfterSpawn {
        fn wrap_child(
            &mut self,
            child: Box<dyn process_wrap::std::ChildWrapper>,
            _core: &process_wrap::std::CommandWrap,
        ) -> std::io::Result<Box<dyn process_wrap::std::ChildWrapper>> {
            *self.0.lock().expect("pid cell") = Some(child.id());
            Err(std::io::Error::other("job setup failed"))
        }
    }

    #[cfg(unix)]
    fn sleeper() -> process_wrap::std::CommandWrap {
        let mut command = Command::new("sleep");
        command.arg("30");
        process_wrap::std::CommandWrap::from(command)
    }

    #[cfg(unix)]
    #[test]
    fn a_spawn_whose_setup_fails_after_the_process_exists_kills_it() {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let mut wrapped = sleeper();
        wrapped
            .wrap(SetupFailureGuard::armed())
            .wrap(FailAfterSpawn(std::sync::Arc::clone(&seen)));
        let error = wrapped.spawn().expect_err("setup fails after the spawn");
        assert!(error.to_string().contains("job setup failed"));
        let pid = seen
            .lock()
            .expect("pid cell")
            .expect("the process existed before setup failed");
        let pid = i32::try_from(pid).expect("pid fits in pid_t");
        // The guard killed and reaped it: the PID no longer names a process.
        let result = unsafe { kill(pid, 0) };
        assert_eq!(result, -1, "the half-set-up process was left running");
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(ESRCH));
    }

    #[cfg(unix)]
    #[test]
    fn a_disarmed_guard_leaves_a_started_process_running() {
        let guard = SetupFailureGuard::armed();
        let mut wrapped = sleeper();
        wrapped.wrap(guard.clone());
        let child = wrapped.spawn().expect("spawn succeeds");
        guard.disarm();
        let pid = i32::try_from(child.id()).expect("pid fits in pid_t");
        drop(child);
        assert_eq!(
            unsafe { kill(pid, 0) },
            0,
            "a disarmed guard must not kill a started process"
        );
        // SAFETY: the test spawned `pid` and has not reaped it.
        unsafe {
            libc::kill(pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }
}
