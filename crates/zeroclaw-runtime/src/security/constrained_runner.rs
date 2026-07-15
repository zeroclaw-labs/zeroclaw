//! Shared constrained process runner for unprivileged, skill-adjacent commands.
//!
//! This is the single place where commands run *on behalf of skill content*
//! (today: `TEST.sh` functional tests; later: adversarial detonation) are
//! executed. It wraps [`std::process::Command`] with:
//!
//! - **Environment scrubbing**: `env_clear()` plus an explicit allowlist, so a
//!   child cannot read the daemon's inherited secrets (API keys, tokens) out of
//!   the process environment.
//! - **Wall-clock timeout**: the child is killed if it runs past the budget.
//! - **Output caps**: captured stdout/stderr are bounded so a runaway command
//!   cannot exhaust memory.
//! - **Working-directory confinement**: the child runs in an explicit directory.
//! - **Sandbox wrapping**: when a non-noop [`Sandbox`] backend is supplied, the
//!   command is wrapped by it before spawning.
//!
//! Source of truth: this runner is the created-here capability. `testing.rs`
//! and any future detonation path call it; there is no second runner.

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

use crate::security::traits::Sandbox;

/// How long `run` waits for the output drainers to deliver their captured text
/// after the child (and, on Unix, its whole process group) has been reaped.
/// Post-kill the pipes close and the drainers report near-instantly; the grace
/// only bounds the pathological case where a stream never reaches EOF, so `run`
/// returns instead of blocking forever.
const DRAIN_GRACE: Duration = Duration::from_secs(5);

/// Default wall-clock budget for a single constrained command.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// Default per-stream (stdout, stderr) capture cap in bytes.
pub const DEFAULT_OUTPUT_CAP_BYTES: usize = 256 * 1024;

/// Environment variables preserved across `env_clear()`. Everything not on
/// this list is stripped so inherited daemon secrets never reach a child.
/// `HOME` is only forwarded when the caller intentionally sets it (see
/// [`ConstrainedRunner::with_home`]).
///
/// The temp-dir vars are on the list because a scrubbed child still needs a
/// writable scratch location: without `TMPDIR`/`TEMP`/`TMP` a `mktemp` or `%TEMP%`
/// in a skill's `TEST.sh` resolves to an unwritable fallback and the test
/// fails for reasons unrelated to the skill. They name locations, not secrets.
const ENV_ALLOWLIST: &[&str] = &[
    "PATH",
    "LANG",
    "LC_ALL",
    "LC_CTYPE",
    "TZ",
    "TERM",
    // Writable scratch location for the child (Unix).
    "TMPDIR",
    // Windows needs these to locate the shell and system libraries.
    "SYSTEMROOT",
    "COMSPEC",
    "PATHEXT",
    "WINDIR",
    // Writable scratch location for the child (Windows); USERPROFILE is the
    // documented GetTempPath fallback when TEMP/TMP are unset.
    "TEMP",
    "TMP",
    "USERPROFILE",
];

/// Outcome of a constrained command execution.
#[derive(Debug, Clone)]
pub struct ConstrainedOutput {
    /// Process exit code, or `None` if the process was killed by a signal or
    /// the timeout before it could set one.
    pub exit_code: Option<i32>,
    /// Captured stdout (UTF-8 lossy), truncated to the output cap.
    pub stdout: String,
    /// Captured stderr (UTF-8 lossy), truncated to the output cap.
    pub stderr: String,
    /// `true` if the command was killed for exceeding the wall-clock budget.
    pub timed_out: bool,
    /// `true` if either stream hit the output cap and was truncated.
    pub output_truncated: bool,
}

impl ConstrainedOutput {
    /// Combined stdout+stderr, matching the legacy `TEST.sh` comparison shape.
    pub fn combined(&self) -> String {
        format!("{}{}", self.stdout, self.stderr)
    }
}

/// Runs a single command under environment, time, output, directory, and
/// sandbox constraints. Construct with the program + args, then configure and
/// [`run`](ConstrainedRunner::run).
pub struct ConstrainedRunner {
    program: PathBuf,
    args: Vec<String>,
    workdir: Option<PathBuf>,
    timeout: Duration,
    output_cap_bytes: usize,
    home: Option<PathBuf>,
    sandbox: Option<Arc<dyn Sandbox>>,
    /// Raw (unquoted-by-std) arguments applied verbatim via `CommandExt::raw_arg`
    /// on Windows, so `cmd.exe`'s own quoting rules survive rather than std's
    /// MSVC-style escaping (which `cmd.exe` misparses).
    #[cfg(windows)]
    windows_raw_args: Vec<std::ffi::OsString>,
    /// Set `CREATE_NO_WINDOW` on Windows so no console flashes for the child.
    #[cfg(windows)]
    windows_no_window: bool,
}

impl ConstrainedRunner {
    /// Create a runner for `program` with default timeout and output caps.
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            workdir: None,
            timeout: DEFAULT_TIMEOUT,
            output_cap_bytes: DEFAULT_OUTPUT_CAP_BYTES,
            home: None,
            sandbox: None,
            #[cfg(windows)]
            windows_raw_args: Vec::new(),
            #[cfg(windows)]
            windows_no_window: false,
        }
    }

    /// Append a single argument.
    pub fn arg(mut self, arg: impl Into<String>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append a raw Windows argument, applied verbatim via `raw_arg` (no std
    /// re-quoting). Used to preserve the `cmd /C "<command>"` quoting contract.
    #[cfg(windows)]
    pub fn windows_raw_arg(mut self, arg: impl Into<std::ffi::OsString>) -> Self {
        self.windows_raw_args.push(arg.into());
        self
    }

    /// Suppress the console window for the Windows child (`CREATE_NO_WINDOW`).
    #[cfg(windows)]
    pub fn windows_no_window(mut self) -> Self {
        self.windows_no_window = true;
        self
    }

    /// Append multiple arguments.
    pub fn args<I, S>(mut self, args: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Confine the child to `dir`.
    pub fn workdir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.workdir = Some(dir.into());
        self
    }

    /// Set the wall-clock budget.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set the per-stream output capture cap in bytes.
    pub fn output_cap_bytes(mut self, cap: usize) -> Self {
        self.output_cap_bytes = cap;
        self
    }

    /// Intentionally forward a `HOME` value to the child (off by default).
    pub fn with_home(mut self, home: impl Into<PathBuf>) -> Self {
        self.home = Some(home.into());
        self
    }

    /// Wrap the command with the given sandbox backend before spawning.
    pub fn sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// Build the fully-constrained [`Command`] (env-cleared + allowlisted,
    /// cwd-confined, sandbox-wrapped). Separated from [`run`](Self::run) so the
    /// constraints can be asserted in tests without spawning.
    fn build_command(&self) -> std::io::Result<Command> {
        let mut cmd = Command::new(&self.program);
        cmd.args(&self.args);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            /// `CREATE_NO_WINDOW` — no console is created for the child.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            for raw in &self.windows_raw_args {
                cmd.raw_arg(raw);
            }
            if self.windows_no_window {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }
        }
        // Run the child as the leader of its own process group so the timeout
        // path can signal the whole group, not just the direct child — a
        // command that backgrounds a grandchild would otherwise keep the
        // stdout/stderr pipes open and stall capture past the budget.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        cmd.env_clear();
        for key in ENV_ALLOWLIST {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        if let Some(home) = &self.home {
            cmd.env("HOME", home);
        }
        if let Some(dir) = &self.workdir {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        if let Some(sandbox) = &self.sandbox {
            sandbox.wrap_command(&mut cmd)?;
        }
        Ok(cmd)
    }

    /// Execute the command under all configured constraints.
    ///
    /// stdout and stderr are drained by dedicated threads *while the child
    /// runs*, so a command that emits more than the OS pipe buffer (~64 KiB)
    /// cannot deadlock waiting for a reader — each drainer keeps consuming to
    /// EOF, retaining only up to the output cap and discarding the rest. The
    /// main thread polls for the wall-clock deadline.
    ///
    /// When the child finishes (or the deadline fires), the *whole process
    /// group* is signalled on Unix — not just the direct child — so a
    /// backgrounded grandchild that inherited the pipes is reaped and the
    /// drainers reach EOF. Collection is then bounded by [`DRAIN_GRACE`], so a
    /// stream that still never closes makes `run` return with truncated output
    /// instead of blocking past the budget forever.
    pub fn run(&self) -> std::io::Result<ConstrainedOutput> {
        let mut child = self.build_command()?.spawn()?;
        let pid = child.id();

        let cap = self.output_cap_bytes;
        let stdout_rx = child.stdout.take().map(|s| drain_capped_async(s, cap));
        let stderr_rx = child.stderr.take().map(|s| drain_capped_async(s, cap));

        let deadline = Instant::now() + self.timeout;
        let mut timed_out = false;
        loop {
            match poll_child_exited(&mut child, pid) {
                // Exited on its own. On Unix the child is detected *without*
                // being reaped (see `poll_child_exited`), so its pid — and thus
                // its process-group id — stays reserved until the sweep below.
                Ok(true) => break,
                Ok(false) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(err) => {
                    // Reap before surfacing the error so we never leak a
                    // running child (and its group) or detached drainers.
                    kill_process_group(pid);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(err);
                }
            }
        }

        // Whether the child exited on its own or hit the deadline, terminate it
        // and sweep its process group *before* the final reap. Killing the child
        // directly is what enforces the timeout on non-Unix (where the
        // process-group sweep is a no-op); the group sweep reaps any Unix
        // backgrounded members so the inherited pipes close and the drainers
        // reach EOF. Doing both before `wait()` keeps the pid reserved for the
        // sweep, so a recycled process-group id can never be signalled.
        kill_process_group(pid);
        let _ = child.kill();
        let exit_code = child.wait().ok().and_then(|status| status.code());

        let (stdout, stdout_truncated) = recv_drain(stdout_rx);
        let (stderr, stderr_truncated) = recv_drain(stderr_rx);

        Ok(ConstrainedOutput {
            exit_code,
            stdout,
            stderr,
            timed_out,
            output_truncated: stdout_truncated || stderr_truncated,
        })
    }
}

/// Whether the child with `pid` has exited, checked *without* reaping it.
///
/// On Unix this uses `waitid(..., WNOWAIT)`: the exited child is left as a
/// still-waitable zombie so its pid — and therefore its process-group id — stays
/// reserved until the caller sweeps the group and reaps explicitly. That closes
/// the window in which a reaped-then-signalled pgid could alias a process group
/// the kernel has since recycled. On non-Unix there is no process-group
/// signalling (and thus no such race), so a plain reaping `try_wait` is used.
#[cfg(unix)]
pub(crate) fn poll_child_exited(
    _child: &mut std::process::Child,
    pid: u32,
) -> std::io::Result<bool> {
    let id = pid as libc::id_t;
    loop {
        // Zeroing the struct lets us distinguish "no child ready" (`si_pid`
        // stays 0 under WNOHANG) from "exited" (`si_pid` set to the child).
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, zeroed `siginfo_t`; `waitid` only writes to
        // it. WNOWAIT leaves the child reapable; WNOHANG makes the call
        // non-blocking. No memory-safety preconditions.
        let ret = unsafe {
            libc::waitid(
                libc::P_PID,
                id,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if ret == 0 {
            // SAFETY: reading a scalar field of the populated siginfo.
            return Ok(unsafe { info.si_pid() } != 0);
        }
        let err = std::io::Error::last_os_error();
        if err.kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        return Err(err);
    }
}

#[cfg(not(unix))]
pub(crate) fn poll_child_exited(
    child: &mut std::process::Child,
    _pid: u32,
) -> std::io::Result<bool> {
    Ok(child.try_wait()?.is_some())
}

/// Signal the process group led by `pid` (SIGKILL) so backgrounded
/// descendants that inherited the child's pipes are reaped. No-op on non-Unix,
/// where the child is killed directly by the caller. Errors (e.g. the group is
/// already gone) are ignored. The caller must not have reaped `pid` yet, so the
/// group id cannot have been recycled onto an unrelated group.
pub(crate) fn kill_process_group(pid: u32) {
    #[cfg(unix)]
    {
        if let Ok(pgid) = i32::try_from(pid) {
            // SAFETY: kill(2) with a negative pid targets the process group;
            // it has no memory-safety preconditions and a bad pgid just fails.
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

/// Collect a drainer's captured text, bounded by [`DRAIN_GRACE`]. Returns
/// `(text, truncated)`; a stream that fails to deliver within the grace is
/// reported as empty-and-truncated so `run` cannot block indefinitely.
fn recv_drain(rx: Option<Receiver<(String, bool)>>) -> (String, bool) {
    match rx {
        Some(rx) => rx
            .recv_timeout(DRAIN_GRACE)
            .unwrap_or_else(|_| (String::new(), true)),
        None => (String::new(), false),
    }
}

/// Spawn a thread that drains `stream` to EOF, keeping at most `cap` bytes and
/// counting the rest so we can flag truncation without buffering it. Draining
/// past the cap is what prevents the child from blocking on a full pipe. The
/// final `(text, truncated)` is delivered over the returned channel; the
/// caller reads it with a bounded timeout so a stream that never closes cannot
/// stall `run`. The thread detaches if that happens and exits when the pipe
/// eventually closes.
fn drain_capped_async<R: Read + Send + 'static>(
    mut stream: R,
    cap: usize,
) -> Receiver<(String, bool)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut kept: Vec<u8> = Vec::new();
        let mut truncated = false;
        let mut buf = [0_u8; 8192];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if kept.len() < cap {
                        let room = cap - kept.len();
                        let take = room.min(n);
                        kept.extend_from_slice(&buf[..take]);
                        if take < n {
                            truncated = true;
                        }
                    } else {
                        truncated = true;
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }
        }
        let _ = tx.send((String::from_utf8_lossy(&kept).into_owned(), truncated));
    });
    rx
}

/// Print a one-line warning that functional tests are running without OS-level
/// sandboxing, so operators know skill commands ran unconfined. Uses the
/// active sandbox's `name()` to decide: only [`super::traits::NoopSandbox`]
/// (`"none"`) triggers the warning.
pub fn warn_if_unsandboxed(sandbox: &Arc<dyn Sandbox>, context: &str) {
    if sandbox.name() != "none" {
        return;
    }
    eprintln!(
        "{}",
        crate::i18n::get_required_cli_string_with_args(
            "cli-skills-test-no-sandbox",
            &[("context", context)],
        )
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::traits::NoopSandbox;

    #[cfg(unix)]
    #[test]
    fn env_is_cleared_except_allowlist() {
        // A canary variable outside the allowlist must not reach the child.
        // SAFETY: single-threaded test; no other thread reads the environment
        // concurrently.
        unsafe {
            std::env::set_var("ZC_TEST_CANARY_SECRET", "leaked-token");
        }
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "echo \"canary=[${ZC_TEST_CANARY_SECRET:-unset}]\""])
            .run()
            .unwrap();
        unsafe {
            std::env::remove_var("ZC_TEST_CANARY_SECRET");
        }
        assert!(
            out.combined().contains("canary=[unset]"),
            "canary env var leaked into child: {}",
            out.combined()
        );
    }

    #[cfg(unix)]
    #[test]
    fn path_is_preserved_so_shell_is_findable() {
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "echo ok"])
            .run()
            .unwrap();
        assert_eq!(out.exit_code, Some(0));
        assert!(out.combined().contains("ok"));
    }

    #[cfg(unix)]
    #[test]
    fn timeout_kills_long_running_child() {
        let started = Instant::now();
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "sleep 30"])
            .timeout(Duration::from_millis(150))
            .run()
            .unwrap();
        assert!(out.timed_out, "long child must be killed for timeout");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "run must return promptly after the timeout, not wait for the child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn output_is_capped() {
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "yes aaaaaaaa | head -c 100000"])
            .output_cap_bytes(1024)
            .run()
            .unwrap();
        assert!(out.output_truncated, "large output must be flagged");
        assert!(
            out.stdout.len() <= 1024,
            "stdout must be truncated to the cap, got {}",
            out.stdout.len()
        );
    }

    #[cfg(unix)]
    #[test]
    fn workdir_confines_child() {
        let tmp = tempfile::tempdir().unwrap();
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "pwd"])
            .workdir(tmp.path())
            .run()
            .unwrap();
        // macOS /tmp is a symlink to /private/tmp; compare canonicalized tails.
        let reported = out.stdout.trim();
        let want = tmp.path().canonicalize().unwrap();
        let got = PathBuf::from(reported).canonicalize().unwrap();
        assert_eq!(got, want);
    }

    #[cfg(unix)]
    #[test]
    fn backgrounded_grandchild_does_not_hang_run() {
        // The shell exits immediately, but it backgrounds a `sleep` that
        // inherits the stdout/stderr pipes. Before the process-group kill,
        // joining the drainers would block for the sleep's full lifetime.
        // `run` must return promptly with the parent's output captured.
        let started = Instant::now();
        let out = ConstrainedRunner::new("sh")
            .args(["-c", "sleep 30 & echo started"])
            .timeout(Duration::from_secs(60))
            .run()
            .unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "run must not block on a backgrounded grandchild holding the pipe"
        );
        assert!(
            out.combined().contains("started"),
            "parent output must still be captured: {:?}",
            out.combined()
        );
    }

    #[test]
    fn sandbox_wrap_is_applied() {
        // A recording sandbox proves wrap_command runs on the built command.
        use std::sync::Arc as StdArc;
        use std::sync::atomic::{AtomicBool, Ordering};

        struct RecordingSandbox {
            wrapped: StdArc<AtomicBool>,
        }
        #[async_trait::async_trait]
        impl Sandbox for RecordingSandbox {
            fn wrap_command(&self, _cmd: &mut Command) -> std::io::Result<()> {
                self.wrapped.store(true, Ordering::SeqCst);
                Ok(())
            }
            fn is_available(&self) -> bool {
                true
            }
            fn name(&self) -> &str {
                "recording"
            }
            fn description(&self) -> &str {
                "test recorder"
            }
        }

        let flag = StdArc::new(AtomicBool::new(false));
        let sandbox: Arc<dyn Sandbox> = Arc::new(RecordingSandbox {
            wrapped: flag.clone(),
        });
        let runner = ConstrainedRunner::new("true").sandbox(sandbox);
        let _ = runner.build_command().unwrap();
        assert!(
            flag.load(Ordering::SeqCst),
            "sandbox.wrap_command must be applied to the built command"
        );
    }

    #[test]
    fn warn_only_for_noop_sandbox() {
        // Smoke: the helper must not panic for either backend. (Its stderr
        // output is a side effect asserted by inspection.)
        let noop: Arc<dyn Sandbox> = Arc::new(NoopSandbox);
        warn_if_unsandboxed(&noop, "skill tests");
    }
}
