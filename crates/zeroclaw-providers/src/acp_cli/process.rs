//! Bounded subprocess lifecycle for one-shot ACP CLI requests.
//!
//! The caller prepares the [`Command`] (program, argv, cwd, and an
//! allowlisted environment). This module owns the rest of the host-side
//! trust boundary:
//!
//! - piped stdio with stderr drained continuously but only bounded metadata
//!   retained, never content;
//! - the child placed in its own process group (Unix) or a kill-on-close Job
//!   Object (Windows);
//! - on success, failure, timeout, or cancellation, termination of the child
//!   process group / Job Object and a bounded reap of the direct child.

use super::protocol::{self, AcpCliFlavor, AcpError, OneshotPrompt};
use serde_json::Value;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::{ChildStderr, ChildStdin, ChildStdout, Command};
use tokio::task::JoinHandle;
use tokio::time::timeout;

#[cfg(windows)]
use process_wrap::tokio::{ChildWrapper, CommandWrap, JobObject, KillOnDrop};
#[cfg(not(windows))]
use tokio::process::Child;

/// Additional bounded time allowed for reaping a terminated child.
const PROCESS_REAP_TIMEOUT: Duration = Duration::from_secs(2);

/// Bounded grace period for a cooperative ACP stdin shutdown.
const STDIN_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(250);

/// Additional bounded time allowed for the stderr drain to observe EOF.
const STDERR_DRAIN_FINISH_TIMEOUT: Duration = Duration::from_millis(250);

/// Stderr is drained continuously, but only bounded metadata is retained.
const MAX_STDERR_BYTES: usize = 65_536;

/// Bounded stderr metadata. Content is never retained.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct StderrSummary {
    pub(crate) bytes_seen: usize,
    pub(crate) truncated: bool,
    pub(crate) read_failed: bool,
}

/// Why a one-shot ACP subprocess request failed.
#[derive(Debug)]
pub(crate) enum AcpSubprocessError {
    /// The program could not be spawned.
    Spawn(std::io::Error),
    /// A piped standard stream (`"stdin"`, `"stdout"`, `"stderr"`) was missing.
    Pipe(&'static str),
    /// The ACP exchange failed.
    Protocol(AcpError),
    /// The complete request exceeded its deadline.
    Timeout,
    /// The request completed but the child could not be reaped.
    Reap,
}

/// Result plus bounded stderr metadata for logging.
pub(crate) struct AcpSubprocessOutcome {
    pub(crate) result: Result<String, AcpSubprocessError>,
    pub(crate) stderr: StderrSummary,
}

struct StderrDrain {
    task: Option<JoinHandle<StderrSummary>>,
}

impl StderrDrain {
    fn start(mut stderr: ChildStderr) -> Self {
        let task = ::zeroclaw_spawn::spawn!(async move {
            let mut summary = StderrSummary::default();
            let mut chunk = [0_u8; 8192];
            loop {
                match stderr.read(&mut chunk).await {
                    Ok(0) => break,
                    Ok(n) => {
                        let remaining = MAX_STDERR_BYTES.saturating_sub(summary.bytes_seen);
                        let retained = remaining.min(n);
                        summary.bytes_seen = summary.bytes_seen.saturating_add(retained);
                        summary.truncated |= retained < n;
                    }
                    Err(_) => {
                        summary.read_failed = true;
                        break;
                    }
                }
            }
            summary
        });
        Self { task: Some(task) }
    }

    async fn finish(mut self) -> StderrSummary {
        let Some(mut task) = self.task.take() else {
            return StderrSummary::default();
        };
        match timeout(STDERR_DRAIN_FINISH_TIMEOUT, &mut task).await {
            Ok(Ok(summary)) => summary,
            Ok(Err(_)) => StderrSummary {
                read_failed: true,
                ..StderrSummary::default()
            },
            Err(_) => {
                task.abort();
                StderrSummary {
                    truncated: true,
                    ..StderrSummary::default()
                }
            }
        }
    }
}

impl Drop for StderrDrain {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// Kill the process group on every exit path. The child is made its own group
/// before spawn, so this does not signal ZeroClaw itself.
#[cfg(unix)]
struct ProcessTreeGuard {
    pgid: Option<nix::unistd::Pid>,
}

#[cfg(unix)]
impl ProcessTreeGuard {
    fn new(child_pid: Option<u32>) -> Self {
        Self {
            pgid: child_pid
                .and_then(|pid| i32::try_from(pid).ok())
                .map(nix::unistd::Pid::from_raw),
        }
    }

    fn kill_now(&self) {
        if let Some(pgid) = self.pgid {
            let _ = nix::sys::signal::killpg(pgid, nix::sys::signal::Signal::SIGKILL);
        }
    }

    fn disarm(&mut self) {
        self.pgid = None;
    }
}

#[cfg(unix)]
impl Drop for ProcessTreeGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// On Windows, the child is assigned to a kill-on-close Job Object. Other
/// non-Unix targets fall back to Tokio's kill-on-drop child handling.
#[cfg(not(unix))]
struct ProcessTreeGuard;

#[cfg(not(unix))]
impl ProcessTreeGuard {
    fn new(_child_pid: Option<u32>) -> Self {
        Self
    }

    fn kill_now(&self) {}

    fn disarm(&mut self) {}
}

#[cfg(windows)]
type ManagedChild = Box<dyn ChildWrapper>;

#[cfg(not(windows))]
type ManagedChild = Child;

#[cfg(windows)]
fn take_child_stdin(child: &mut ManagedChild) -> Option<ChildStdin> {
    child.stdin().take()
}

#[cfg(not(windows))]
fn take_child_stdin(child: &mut ManagedChild) -> Option<ChildStdin> {
    child.stdin.take()
}

#[cfg(windows)]
fn take_child_stdout(child: &mut ManagedChild) -> Option<ChildStdout> {
    child.stdout().take()
}

#[cfg(not(windows))]
fn take_child_stdout(child: &mut ManagedChild) -> Option<ChildStdout> {
    child.stdout.take()
}

#[cfg(windows)]
fn take_child_stderr(child: &mut ManagedChild) -> Option<ChildStderr> {
    child.stderr().take()
}

#[cfg(not(windows))]
fn take_child_stderr(child: &mut ManagedChild) -> Option<ChildStderr> {
    child.stderr.take()
}

async fn terminate_child_tree(child: &mut ManagedChild, guard: &mut ProcessTreeGuard) -> bool {
    guard.kill_now();
    let _ = child.start_kill();
    match timeout(PROCESS_REAP_TIMEOUT, child.wait()).await {
        Ok(Ok(_)) => {
            guard.disarm();
            true
        }
        Ok(Err(_)) | Err(_) => false,
    }
}

/// Spawn the prepared command and run one bounded ACP prompt against it.
///
/// `cmd` must already carry the program, argv, cwd, and environment. This
/// function configures stdio and process-tree containment, applies
/// `deadline` to the complete exchange, and always terminates and reaps the
/// child before returning.
pub(crate) async fn run_oneshot_subprocess<A>(
    mut cmd: Command,
    deadline: Duration,
    request: &OneshotPrompt<'_>,
    select_auth: A,
) -> AcpSubprocessOutcome
where
    A: FnOnce(&Value) -> Result<Option<Value>, AcpError>,
{
    #[cfg(not(windows))]
    cmd.kill_on_drop(true);
    cmd.stdin(std::process::Stdio::piped());
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());
    #[cfg(unix)]
    cmd.process_group(0);

    #[cfg(windows)]
    let spawn_result = {
        let mut command = CommandWrap::from(cmd);
        command.wrap(KillOnDrop).wrap(JobObject);
        command.spawn()
    };
    #[cfg(not(windows))]
    let spawn_result = cmd.spawn();

    let failed = |error| AcpSubprocessOutcome {
        result: Err(error),
        stderr: StderrSummary::default(),
    };

    let mut child = match spawn_result {
        Ok(child) => child,
        Err(error) => return failed(AcpSubprocessError::Spawn(error)),
    };

    let mut process_tree = ProcessTreeGuard::new(child.id());
    let Some(mut stdin) = take_child_stdin(&mut child) else {
        return failed(AcpSubprocessError::Pipe("stdin"));
    };
    let Some(stdout) = take_child_stdout(&mut child) else {
        return failed(AcpSubprocessError::Pipe("stdout"));
    };
    let Some(stderr) = take_child_stderr(&mut child) else {
        return failed(AcpSubprocessError::Pipe("stderr"));
    };
    let stderr_drain = StderrDrain::start(stderr);

    let request_result = timeout(
        deadline,
        protocol::run_oneshot_prompt(&mut stdin, stdout, request, select_auth),
    )
    .await;

    // Closing stdin tells a cooperative ACP server it can stop. We still
    // terminate the complete tree because a completed turn may have left
    // tool descendants running.
    let _ = timeout(STDIN_SHUTDOWN_TIMEOUT, stdin.shutdown()).await;
    drop(stdin);
    let reaped = terminate_child_tree(&mut child, &mut process_tree).await;
    let stderr = stderr_drain.finish().await;

    let result = match request_result {
        Ok(Ok(text)) if reaped => Ok(text),
        Ok(Ok(_)) => Err(AcpSubprocessError::Reap),
        Ok(Err(error)) => Err(AcpSubprocessError::Protocol(error)),
        Err(_) => Err(AcpSubprocessError::Timeout),
    };
    AcpSubprocessOutcome { result, stderr }
}

/// Record a failed ACP subprocess phase with bounded stderr metadata only.
/// Child stderr content and raw ACP frames are never logged.
pub(crate) fn log_acp_failure(
    flavor: AcpCliFlavor,
    phase: &'static str,
    error_key: &str,
    stderr: StderrSummary,
) {
    ::zeroclaw_log::record!(
        ERROR,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(::serde_json::json!({
                "phase": phase,
                "error_key": error_key,
                "stderr_bytes_seen": stderr.bytes_seen,
                "stderr_truncated": stderr.truncated,
                "stderr_read_failed": stderr.read_failed,
            })),
        &format!("{}: ACP subprocess failed", flavor.key_prefix)
    );
}
