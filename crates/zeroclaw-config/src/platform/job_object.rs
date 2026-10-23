//! Ownership of a process tree on Windows.
//!
//! A daemon spawns helpers that hold copies of its handles: a browser session,
//! MCP servers, shell commands. When the daemon is killed rather than shut
//! down, those helpers survive and keep a listening socket's handles open, so
//! the port stays bound until they exit. A job object makes the kernel the
//! owner of the tree instead: every descendant joins the job at creation, and
//! when the job's last handle closes, which happens when the daemon exits by
//! any route, every member is terminated.

#[cfg(windows)]
pub use win32::{KillOnCloseJob, confine_process_tree};

#[cfg(windows)]
mod win32 {
    use std::io;
    use std::mem::size_of;
    use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT_BREAKAWAY_OK,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;
    use windows::core::BOOL;

    /// An unnamed job object that terminates every member process when its
    /// last handle closes. Members may spawn a child outside the job with
    /// `CREATE_BREAKAWAY_FROM_JOB`; the child then belongs to the enclosing
    /// jobs, if any, as before.
    #[derive(Debug)]
    pub struct KillOnCloseJob(OwnedHandle);

    impl KillOnCloseJob {
        /// Create an empty job.
        pub fn create() -> io::Result<Self> {
            // SAFETY: no security attributes and no name, so the call reads
            // nothing from this process; the handle it returns is owned here
            // and non-inheritable.
            let handle = unsafe { CreateJobObjectW(None, None) }
                .map_err(|error| io::Error::other(format!("CreateJobObjectW: {error}")))?;
            // SAFETY: CreateJobObjectW returned an owned, valid handle.
            let job = Self(unsafe { OwnedHandle::from_raw_handle(handle.0) });
            let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            limits.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_BREAKAWAY_OK;
            // SAFETY: `limits` is a fully initialized structure of the length
            // passed, and the job handle is open for the call.
            unsafe {
                SetInformationJobObject(
                    job.handle(),
                    JobObjectExtendedLimitInformation,
                    (&raw const limits).cast(),
                    size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                )
            }
            .map_err(|error| io::Error::other(format!("SetInformationJobObject: {error}")))?;
            Ok(job)
        }

        /// Add `process` to the job; the processes it spawns from now on
        /// join too. A process that is already in another job nests this
        /// job under that one.
        pub fn assign(&self, process: &impl AsHandle) -> io::Result<()> {
            self.assign_handle(HANDLE(process.as_handle().as_raw_handle()))
        }

        /// Add this process to the job.
        pub fn assign_current_process(&self) -> io::Result<()> {
            // SAFETY: the current-process pseudo handle is always valid and
            // is never closed.
            self.assign_handle(unsafe { GetCurrentProcess() })
        }

        /// Whether `process` is a member of the job.
        pub fn contains(&self, process: &impl AsHandle) -> io::Result<bool> {
            let mut member = BOOL(0);
            // SAFETY: both handles are open for the call and `member` is a
            // valid out-pointer.
            unsafe {
                IsProcessInJob(
                    HANDLE(process.as_handle().as_raw_handle()),
                    Some(self.handle()),
                    &mut member,
                )
            }
            .map_err(|error| io::Error::other(format!("IsProcessInJob: {error}")))?;
            Ok(member.as_bool())
        }

        /// Keep the job open until this process ends. Windows closes the
        /// handle at process exit, and that close is what terminates the
        /// members; closing it any earlier would terminate them, and this
        /// process if it is a member, at once.
        pub fn keep_for_process_lifetime(self) {
            std::mem::forget(self);
        }

        fn assign_handle(&self, process: HANDLE) -> io::Result<()> {
            // SAFETY: the job handle is open and `process` is a valid process
            // handle for the call.
            unsafe { AssignProcessToJobObject(self.handle(), process) }
                .map_err(|error| io::Error::other(format!("AssignProcessToJobObject: {error}")))
        }

        fn handle(&self) -> HANDLE {
            HANDLE(self.0.as_raw_handle())
        }
    }

    /// Place this process, and so every process it spawns from now on, in a
    /// job that the kernel terminates when this process ends. The job allows
    /// breakaway, so a child meant to outlive this process can opt out with
    /// `CREATE_BREAKAWAY_FROM_JOB` at creation.
    pub fn confine_process_tree() -> io::Result<()> {
        let job = KillOnCloseJob::create()?;
        job.assign_current_process()?;
        job.keep_for_process_lifetime();
        Ok(())
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use std::os::windows::process::CommandExt;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};
    use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0};
    use windows::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
    };

    const SETTLE: Duration = Duration::from_secs(10);

    /// A process that runs for a minute unless something stops it.
    fn long_lived_command() -> Command {
        let mut command = Command::new("ping");
        command
            .args(["-n", "60", "127.0.0.1"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn settles(mut condition: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + SETTLE;
        loop {
            if condition() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// Whether process `pid` has exited or never existed.
    fn has_exited(pid: u32) -> bool {
        // SAFETY: OpenProcess reads no memory of this process.
        let Ok(process) = (unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        }) else {
            return true;
        };
        // SAFETY: OpenProcess returned an owned, valid handle.
        let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
        // SAFETY: the handle is open for the call.
        let wait = unsafe { WaitForSingleObject(HANDLE(process.as_raw_handle()), 0) };
        wait == WAIT_OBJECT_0
    }

    fn terminate(pid: u32) {
        // SAFETY: OpenProcess reads no memory of this process.
        if let Ok(process) = unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
            // SAFETY: OpenProcess returned an owned, valid handle.
            let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
            // SAFETY: the handle is open for the call.
            let _ = unsafe { TerminateProcess(HANDLE(process.as_raw_handle()), 1) };
        }
    }

    #[test]
    fn a_member_dies_when_the_job_closes() {
        let job = KillOnCloseJob::create().expect("create a job");
        let mut member = long_lived_command().spawn().expect("spawn a member");
        job.assign(&member).expect("assign the member");
        assert!(job.contains(&member).expect("query membership"));

        drop(job);

        let exited = settles(|| member.try_wait().expect("poll the member").is_some());
        if !exited {
            let _ = member.kill();
        }
        assert!(exited, "closing the job should terminate its member");
    }

    #[test]
    fn a_process_outside_the_job_is_not_a_member() {
        let job = KillOnCloseJob::create().expect("create a job");
        let mut outsider = long_lived_command().spawn().expect("spawn an outsider");
        let membership = job.contains(&outsider);
        outsider.kill().expect("terminate the outsider");
        outsider.wait().expect("reap the outsider");
        assert!(!membership.expect("query membership"));
    }

    /// The role of a copy of this test binary that stands in for a daemon:
    /// it confines itself, spawns a long-lived grandchild, reports the
    /// grandchild's id, and waits to be killed.
    const HELPER_ROLE: &str = "ZEROCLAW_JOB_OBJECT_HELPER_ROLE";
    const CONFINED: &str = "confined";
    const BREAKAWAY: &str = "breakaway";

    fn run_helper(role: &str) {
        confine_process_tree().expect("the helper confines itself");
        let mut command = long_lived_command();
        if role == BREAKAWAY {
            command.creation_flags(CREATE_BREAKAWAY_FROM_JOB.0);
        }
        let mut grandchild = command.spawn().expect("the helper spawns its grandchild");
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "grandchild={}", grandchild.id()).expect("report the grandchild");
        stdout.flush().expect("flush the report");
        // The test kills this helper long before the grandchild finishes.
        let _ = grandchild.wait();
    }

    /// Run `test` in a fresh copy of this binary with `role`, and return the
    /// helper and the id of the grandchild it spawned.
    fn spawn_helper(test: &str, role: &str) -> (Child, u32) {
        let mut helper = Command::new(std::env::current_exe().expect("this test binary"))
            .args(["--exact", test, "--nocapture", "--test-threads=1"])
            .env(HELPER_ROLE, role)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn the helper");
        let stdout = helper.stdout.take().expect("the helper's stdout is piped");
        let mut transcript = String::new();
        let mut grandchild = None;
        // The harness prints the test's name on the same line, so the report
        // may start anywhere in it.
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if let Some((_, report)) = line.split_once("grandchild=") {
                let digits: String = report.chars().take_while(char::is_ascii_digit).collect();
                if let Ok(pid) = digits.parse::<u32>() {
                    grandchild = Some(pid);
                    break;
                }
            }
            transcript.push_str(&line);
            transcript.push('\n');
        }
        let Some(grandchild) = grandchild else {
            let _ = helper.kill();
            let _ = helper.wait();
            panic!("the helper reported no grandchild:\n{transcript}");
        };
        (helper, grandchild)
    }

    #[test]
    fn a_confined_parent_takes_its_tree_down_when_killed() {
        if let Some(role) = std::env::var_os(HELPER_ROLE) {
            run_helper(&role.to_string_lossy());
            return;
        }
        let (mut helper, grandchild) = spawn_helper(
            "platform::job_object::tests::a_confined_parent_takes_its_tree_down_when_killed",
            CONFINED,
        );
        assert!(
            !has_exited(grandchild),
            "the grandchild runs before the kill"
        );

        helper.kill().expect("kill the helper");
        helper.wait().expect("reap the helper");

        let gone = settles(|| has_exited(grandchild));
        if !gone {
            terminate(grandchild);
        }
        assert!(
            gone,
            "killing the confined parent should take its grandchild down"
        );
    }

    #[test]
    fn a_breakaway_child_survives_its_parent() {
        if let Some(role) = std::env::var_os(HELPER_ROLE) {
            run_helper(&role.to_string_lossy());
            return;
        }
        let (mut helper, grandchild) = spawn_helper(
            "platform::job_object::tests::a_breakaway_child_survives_its_parent",
            BREAKAWAY,
        );
        assert!(
            !has_exited(grandchild),
            "the grandchild runs before the kill"
        );

        helper.kill().expect("kill the helper");
        helper.wait().expect("reap the helper");

        std::thread::sleep(Duration::from_secs(1));
        let survived = !has_exited(grandchild);
        terminate(grandchild);
        assert!(
            survived,
            "a child that broke away should outlive its parent"
        );
    }
}
