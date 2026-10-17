//! Resident-memory sampling for one owned native command and its visible descendants.

use std::collections::{HashMap, HashSet};
use std::io;
use std::process::ExitStatus;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessStatus, ProcessesToUpdate, System};
use tokio::process::{Child, ChildStderr, ChildStdout, Command};

const SAMPLE_INTERVAL: Duration = Duration::from_millis(100);
const BYTES_PER_MB: u64 = 1024 * 1024;

/// The caller owns Unix process-group cleanup, including cancellation and monitor failures.
/// Windows supervision owns a Job Object; disabled supervision keeps ordinary child waiting.
pub(super) struct ManagedChild {
    #[cfg(not(windows))]
    child: Child,
    #[cfg(windows)]
    child: WindowsChild,
}

#[cfg(windows)]
enum WindowsChild {
    Ordinary(Child),
    Job(Box<dyn process_wrap::tokio::ChildWrapper>),
}

#[cfg(windows)]
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let WindowsChild::Job(child) = &mut self.child {
            // process-wrap 9.0 cannot see KillOnDrop during JobObject setup.
            // Terminate the owned job explicitly, even after the root is reaped.
            let _ = child.start_kill();
        }
    }
}

#[derive(Debug)]
pub(super) enum MemoryWaitError {
    Io(io::Error),
    Exceeded { limit_mb: u64, rss_bytes: u64 },
    Unavailable { diagnostic: String },
}

impl ManagedChild {
    pub(super) fn spawn(mut command: Command, memory_mb: u64) -> io::Result<Self> {
        command.kill_on_drop(true);
        #[cfg(windows)]
        {
            use process_wrap::tokio::{CommandWrap, JobObject, KillOnDrop};
            let child = if memory_mb == 0 {
                WindowsChild::Ordinary(command.spawn()?)
            } else {
                let mut command = CommandWrap::from(command);
                command
                    .wrap(KillOnDrop)
                    .wrap(crate::service::WindowsSpawnFailureGuard)
                    .wrap(JobObject);
                WindowsChild::Job(command.spawn()?)
            };
            Ok(Self { child })
        }
        #[cfg(not(windows))]
        {
            let _ = memory_mb;
            Ok(Self {
                child: command.spawn()?,
            })
        }
    }

    pub(super) fn id(&self) -> Option<u32> {
        #[cfg(not(windows))]
        {
            self.child.id()
        }
        #[cfg(windows)]
        match &self.child {
            WindowsChild::Ordinary(child) => child.id(),
            WindowsChild::Job(child) => child.id(),
        }
    }

    pub(super) fn stdout(&mut self) -> &mut Option<ChildStdout> {
        #[cfg(not(windows))]
        {
            &mut self.child.stdout
        }
        #[cfg(windows)]
        match &mut self.child {
            WindowsChild::Ordinary(child) => &mut child.stdout,
            WindowsChild::Job(child) => child.stdout(),
        }
    }

    pub(super) fn stderr(&mut self) -> &mut Option<ChildStderr> {
        #[cfg(not(windows))]
        {
            &mut self.child.stderr
        }
        #[cfg(windows)]
        match &mut self.child {
            WindowsChild::Ordinary(child) => &mut child.stderr,
            WindowsChild::Job(child) => child.stderr(),
        }
    }

    pub(super) fn start_kill(&mut self) -> io::Result<()> {
        #[cfg(not(windows))]
        {
            self.child.start_kill()
        }
        #[cfg(windows)]
        match &mut self.child {
            WindowsChild::Ordinary(child) => child.start_kill(),
            WindowsChild::Job(child) => child.start_kill(),
        }
    }

    pub(super) async fn wait(&mut self) -> io::Result<ExitStatus> {
        #[cfg(not(windows))]
        {
            self.child.wait().await
        }
        #[cfg(windows)]
        match &mut self.child {
            WindowsChild::Ordinary(child) => child.wait().await,
            // The watchdog covers the root lifetime on every platform. Keep
            // the Job Object for containment, but do not use its wait-for-all
            // completion-port wrapper. Dropping ManagedChild stops leftovers.
            WindowsChild::Job(child) => child.inner_mut().wait().await,
        }
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        #[cfg(not(windows))]
        {
            self.child.try_wait()
        }
        #[cfg(windows)]
        match &mut self.child {
            WindowsChild::Ordinary(child) => child.try_wait(),
            WindowsChild::Job(child) => child.inner_mut().try_wait(),
        }
    }

    /// Monitor failure leaves the root unreaped so the caller can kill its owned group/job
    /// before reaping, then report the original memory failure regardless of cleanup status.
    pub(super) async fn wait_with_memory(
        &mut self,
        memory_mb: u64,
    ) -> Result<ExitStatus, MemoryWaitError> {
        if memory_mb == 0 {
            return self.wait().await.map_err(MemoryWaitError::Io);
        }
        let Some(root) = self.id().map(Pid::from_u32) else {
            return self.wait().await.map_err(MemoryWaitError::Io);
        };
        #[cfg(windows)]
        if matches!(self.child, WindowsChild::Ordinary(_)) {
            return Err(MemoryWaitError::Unavailable {
                diagnostic: "memory supervision requires an owned Job Object".into(),
            });
        }
        let limit_bytes = memory_mb.saturating_mul(BYTES_PER_MB);
        let cancellation = SampleCancellation(Arc::new(AtomicBool::new(false)));
        let mut root_start_time = None;
        loop {
            if self.try_wait().map_err(MemoryWaitError::Io)?.is_some() {
                return self.wait().await.map_err(MemoryWaitError::Io);
            }
            let cancelled = Arc::clone(&cancellation.0);
            // Do not poll/reap the root while sampling. On Unix its unreaped PID cannot
            // be recycled, including when it exits during this blocking observation.
            let sample = tokio::task::spawn_blocking(move || {
                sample_memory(root, root_start_time, &cancelled)
            })
            .await;
            // A missing/zombie root is normal completion when the owned wait confirms it.
            if self.try_wait().map_err(MemoryWaitError::Io)?.is_some() {
                return self.wait().await.map_err(MemoryWaitError::Io);
            }
            let (rss_bytes, start_time) = sample
                .map_err(|error| MemoryWaitError::Unavailable {
                    diagnostic: format!("memory sampler failed: {error}"),
                })?
                .map_err(|diagnostic| MemoryWaitError::Unavailable {
                    diagnostic: diagnostic.into(),
                })?;
            root_start_time = Some(start_time);
            if rss_bytes > limit_bytes {
                return Err(MemoryWaitError::Exceeded {
                    limit_mb: memory_mb,
                    rss_bytes,
                });
            }
            tokio::time::sleep(SAMPLE_INTERVAL).await;
        }
    }
}

struct SampleCancellation(Arc<AtomicBool>);

impl Drop for SampleCancellation {
    fn drop(&mut self) {
        // spawn_blocking cannot abort an in-progress sysinfo call. It may finish its
        // current observation, but cancellation prevents further phases or samples.
        self.0.store(true, Ordering::Release);
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct ProcessIdentity {
    parent: Option<Pid>,
    start_time: u64,
}

struct NativeMemory {
    pid: Pid,
    identity: ProcessIdentity,
    rss_bytes: u64,
}

fn resident_memory_with_fallback(
    pid: Pid,
    identity: ProcessIdentity,
    rss_bytes: u64,
    native_read: impl FnOnce(Pid, ProcessIdentity) -> Result<Option<NativeMemory>, &'static str>,
) -> Result<Option<u64>, &'static str> {
    if rss_bytes != 0 {
        return Ok(Some(rss_bytes));
    }
    // sysinfo represents both a failed memory read and a successful zero as zero.
    // The native response supplies current RSS and pins it to the observed identity.
    let Some(native) = native_read(pid, identity)? else {
        return Ok(None);
    };
    if native.pid != pid || native.identity != identity {
        return Ok(None);
    }
    Ok(Some(native.rss_bytes))
}

#[cfg(target_os = "macos")]
fn native_resident_memory(
    pid: Pid,
    _identity: ProcessIdentity,
) -> Result<Option<NativeMemory>, &'static str> {
    // SAFETY: proc_taskallinfo contains only integer fields and arrays of integers.
    let mut info: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;
    // SAFETY: info is writable for exactly size bytes. Read identity and RSS together.
    let returned = unsafe {
        libc::proc_pidinfo(
            pid.as_u32() as libc::c_int,
            libc::PROC_PIDTASKALLINFO,
            0,
            std::ptr::from_mut(&mut info).cast(),
            size,
        )
    };
    if returned == 0 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        return Ok(None);
    }
    macos_memory_response(&info, returned).map(Some)
}

#[cfg(target_os = "macos")]
fn macos_memory_response(
    info: &libc::proc_taskallinfo,
    returned: libc::c_int,
) -> Result<NativeMemory, &'static str> {
    if returned != std::mem::size_of::<libc::proc_taskallinfo>() as libc::c_int {
        return Err("native resident memory read failed or was incomplete");
    }
    Ok(NativeMemory {
        pid: Pid::from_u32(info.pbsd.pbi_pid),
        identity: ProcessIdentity {
            parent: (info.pbsd.pbi_ppid != 0).then(|| Pid::from_u32(info.pbsd.pbi_ppid)),
            start_time: info.pbsd.pbi_start_tvsec,
        },
        rss_bytes: info.ptinfo.pti_resident_size,
    })
}

#[cfg(target_os = "linux")]
fn native_resident_memory(
    pid: Pid,
    _identity: ProcessIdentity,
) -> Result<Option<NativeMemory>, &'static str> {
    let stat = match std::fs::read_to_string(format!("/proc/{}/stat", pid.as_u32())) {
        Ok(stat) => stat,
        Err(error) if linux_process_departed_error(&error) => return Ok(None),
        Err(_) => return Err("native resident memory read failed"),
    };
    // SAFETY: sysconf only queries these process-independent numeric system constants.
    let (page_size, clock_ticks) = unsafe {
        (
            libc::sysconf(libc::_SC_PAGESIZE),
            libc::sysconf(libc::_SC_CLK_TCK),
        )
    };
    if page_size <= 0 || clock_ticks <= 0 {
        return Err("native resident memory units are not observable");
    }
    linux_memory_response(
        &stat,
        page_size as u64,
        clock_ticks as u64,
        System::boot_time(),
    )
    .map(Some)
}

#[cfg(target_os = "linux")]
fn linux_process_departed_error(error: &io::Error) -> bool {
    // procfs can report ESRCH if the task exits between opening stat and reading it.
    error.kind() == io::ErrorKind::NotFound || error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(target_os = "linux")]
fn linux_memory_response(
    stat: &str,
    page_size: u64,
    clock_ticks: u64,
    boot_time: u64,
) -> Result<NativeMemory, &'static str> {
    let invalid = "native resident memory read failed or was incomplete";
    if page_size == 0 || clock_ticks == 0 {
        return Err(invalid);
    }
    let (pid, comm_and_fields) = stat.split_once(" (").ok_or(invalid)?;
    // comm may itself contain spaces and parentheses; numeric fields follow its last ')'.
    let (_, fields) = comm_and_fields.rsplit_once(") ").ok_or(invalid)?;
    let fields: Vec<_> = fields.split_whitespace().collect();
    // Only fields through RSS are needed; older kernels omit later additions.
    if !stat.ends_with('\n') || fields.len() < 22 || fields[0].len() != 1 {
        return Err(invalid);
    }
    let pid = pid.parse::<u32>().map_err(|_| invalid)?;
    let parent = fields[1].parse::<u32>().map_err(|_| invalid)?;
    let start_ticks = fields[19].parse::<u64>().map_err(|_| invalid)?;
    let rss_pages = fields[21].parse::<u64>().map_err(|_| invalid)?;
    Ok(NativeMemory {
        pid: Pid::from_u32(pid),
        identity: ProcessIdentity {
            parent: (parent != 0).then(|| Pid::from_u32(parent)),
            // Match sysinfo's whole-second process start time conversion.
            start_time: (start_ticks / clock_ticks).saturating_add(boot_time),
        },
        rss_bytes: rss_pages.checked_mul(page_size).ok_or(invalid)?,
    })
}

#[cfg(windows)]
fn native_resident_memory(
    pid: Pid,
    identity: ProcessIdentity,
) -> Result<Option<NativeMemory>, &'static str> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0,
    };
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_VM_READ, WaitForSingleObject,
    };

    // SAFETY: open a query-only handle, then use that same process object for both reads.
    let handle = match unsafe {
        OpenProcess(
            PROCESS_QUERY_INFORMATION | PROCESS_VM_READ | PROCESS_SYNCHRONIZE,
            false,
            pid.as_u32(),
        )
    } {
        Ok(handle) => handle,
        Err(error)
            if pid.as_u32() != 0
                && error.code()
                    == windows::core::HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) =>
        {
            return Ok(None);
        }
        Err(_) => return Err("native resident memory query handle is not observable"),
    };
    // A signaled process handle proves exit without confusing access failures with departure.
    let departed = || unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
    let result = (|| {
        if departed() {
            return Ok(None);
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        // SAFETY: all output pointers are writable FILETIMEs and handle remains open.
        unsafe { GetProcessTimes(handle, &mut creation, &mut exit, &mut kernel, &mut user) }
            .map_err(|_| "native process identity is not observable")?;
        let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
        let mut memory = PROCESS_MEMORY_COUNTERS {
            cb: size,
            ..Default::default()
        };
        // SAFETY: memory is writable for size bytes and refers to the same live handle.
        unsafe { GetProcessMemoryInfo(handle, &mut memory, size) }
            .map_err(|_| "native resident memory read failed")?;
        let creation_ticks =
            (u64::from(creation.dwHighDateTime) << 32) | u64::from(creation.dwLowDateTime);
        let start_time = (creation_ticks / 10_000_000)
            .checked_sub(11_644_473_600)
            .ok_or("native process identity is not observable")?;
        Ok(Some(NativeMemory {
            pid,
            identity: ProcessIdentity {
                // Parent was checked in the refreshed snapshot; creation pins this handle.
                parent: identity.parent,
                start_time,
            },
            rss_bytes: memory.WorkingSetSize as u64,
        }))
    })();
    let result = if result.is_err() && departed() {
        Ok(None)
    } else {
        result
    };
    // SAFETY: every successful OpenProcess above reaches this close, including failed reads.
    unsafe { CloseHandle(handle) }.map_err(|_| "native memory query handle close failed")?;
    result
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn native_resident_memory(
    _pid: Pid,
    _identity: ProcessIdentity,
) -> Result<Option<NativeMemory>, &'static str> {
    Err("native resident memory is not supported on this platform")
}

fn sample_memory(
    root: Pid,
    root_start_time: Option<u64>,
    cancelled: &AtomicBool,
) -> Result<(u64, u64), &'static str> {
    sample_memory_with_reader(
        root,
        root_start_time,
        cancelled,
        |pid, identity, rss_bytes| {
            resident_memory_with_fallback(pid, identity, rss_bytes, native_resident_memory)
        },
    )
}

fn sample_memory_with_reader(
    root: Pid,
    root_start_time: Option<u64>,
    cancelled: &AtomicBool,
    mut read_memory: impl FnMut(Pid, ProcessIdentity, u64) -> Result<Option<u64>, &'static str>,
) -> Result<(u64, u64), &'static str> {
    if cancelled.load(Ordering::Acquire) {
        return Err("memory observation cancelled");
    }
    // A fresh snapshot prevents a failed memory refresh from reusing stale RSS.
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().without_tasks(),
    );
    let root_process = system.process(root).ok_or("owned root is not observable")?;
    let start_time = root_process.start_time();
    if root_start_time.is_some_and(|expected| expected != start_time) {
        return Err("owned root identity changed");
    }
    let mut children = HashMap::<Pid, Vec<Pid>>::new();
    for (pid, process) in system.processes() {
        if let Some(parent) = process.parent() {
            children.entry(parent).or_default().push(*pid);
        }
    }
    let mut discovered = HashSet::from([root]);
    let mut pending = vec![root];
    while let Some(pid) = pending.pop() {
        if let Some(descendants) = children.get(&pid) {
            for child in descendants {
                if discovered.insert(*child) {
                    pending.push(*child);
                }
            }
        }
    }
    let identities: HashMap<_, _> = discovered
        .iter()
        .filter_map(|pid| {
            system.process(*pid).map(|process| {
                (
                    *pid,
                    ProcessIdentity {
                        parent: process.parent(),
                        start_time: process.start_time(),
                    },
                )
            })
        })
        .collect();
    if cancelled.load(Ordering::Acquire) {
        return Err("memory observation cancelled");
    }
    let pids: Vec<_> = discovered.into_iter().collect();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&pids),
        true,
        ProcessRefreshKind::nothing().without_tasks().with_memory(),
    );
    if cancelled.load(Ordering::Acquire) {
        return Err("memory observation cancelled");
    }
    let root_process = system.process(root).ok_or("owned root is not observable")?;
    if root_process.start_time() != start_time {
        return Err("owned root identity changed");
    }
    let mut rss_bytes = 0_u64;
    let mut pending = vec![root];
    let mut counted = HashSet::new();
    while let Some(pid) = pending.pop() {
        if !counted.insert(pid) {
            continue;
        }
        let Some(identity) = identities.get(&pid) else {
            continue;
        };
        let Some(process) = system.process(pid) else {
            continue;
        };
        let refreshed = ProcessIdentity {
            parent: process.parent(),
            start_time: process.start_time(),
        };
        // Never charge a process that replaced a descendant between the two reads.
        if refreshed != *identity {
            if pid == root {
                return Err("owned root identity changed");
            }
            continue;
        }
        let memory =
            if pid != root && process.memory() == 0 && process.status() == ProcessStatus::Zombie {
                Some(0)
            } else {
                read_memory(pid, *identity, process.memory())?
            };
        let Some(memory) = memory else {
            if pid == root {
                return Err("owned root identity changed");
            }
            continue;
        };
        rss_bytes = rss_bytes.saturating_add(memory);
        // An unchanged descendant is eligible only through unchanged, visible parents.
        if let Some(descendants) = children.get(&pid) {
            pending.extend(descendants);
        }
    }
    Ok((rss_bytes, start_time))
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos", windows)))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::io::{AsyncBufReadExt, BufReader};

    const FIXTURE_ENV: &str = "ZEROCLAW_RESIDENT_MEMORY_FIXTURE";
    const FIXTURE_TEST: &str = "tools::subprocess_memory::tests::memory_fixture";
    // Bound aggregate test-host pressure even when the surrounding suite runs in parallel.
    static FIXTURE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    #[test]
    fn successful_native_read_accepts_zero_and_positive_resident_memory() {
        let pid = Pid::from_u32(42);
        let identity = ProcessIdentity {
            parent: Some(Pid::from_u32(7)),
            start_time: 100,
        };
        for rss_bytes in [0, 4096] {
            assert_eq!(
                resident_memory_with_fallback(pid, identity, 0, |queried, expected| {
                    assert_eq!(queried, pid);
                    assert!(expected == identity);
                    Ok(Some(NativeMemory {
                        pid,
                        identity,
                        rss_bytes,
                    }))
                }),
                Ok(Some(rss_bytes))
            );
        }
    }

    #[test]
    fn failed_native_read_does_not_accept_sysinfo_zero() {
        let identity = ProcessIdentity {
            parent: None,
            start_time: 100,
        };
        assert_eq!(
            resident_memory_with_fallback(Pid::from_u32(42), identity, 0, |_, _| {
                Err("native resident memory read failed")
            }),
            Err("native resident memory read failed")
        );
    }

    #[test]
    fn native_identity_mismatch_is_not_counted() {
        let pid = Pid::from_u32(42);
        let identity = ProcessIdentity {
            parent: Some(Pid::from_u32(7)),
            start_time: 100,
        };
        for native in [
            NativeMemory {
                pid: Pid::from_u32(43),
                identity,
                rss_bytes: 4096,
            },
            NativeMemory {
                pid,
                identity: ProcessIdentity {
                    parent: Some(Pid::from_u32(8)),
                    ..identity
                },
                rss_bytes: 4096,
            },
            NativeMemory {
                pid,
                identity: ProcessIdentity {
                    start_time: 101,
                    ..identity
                },
                rss_bytes: 4096,
            },
        ] {
            assert_eq!(
                resident_memory_with_fallback(pid, identity, 0, |_, _| Ok(Some(native))),
                Ok(None)
            );
        }
    }

    #[test]
    fn positive_sysinfo_resident_memory_bypasses_native_read() {
        let identity = ProcessIdentity {
            parent: None,
            start_time: 100,
        };
        assert_eq!(
            resident_memory_with_fallback(Pid::from_u32(42), identity, 4096, |_, _| {
                panic!("positive sysinfo RSS must bypass the native read")
            }),
            Ok(Some(4096))
        );
    }

    #[tokio::test]
    async fn descendant_departure_after_snapshot_is_not_a_monitor_failure() {
        let _fixture_lock = FIXTURE_LOCK.lock().await;
        let root = Pid::from_u32(std::process::id());
        for depart in [true, false] {
            let mut child = fixture_command("await-release")
                .stdin(Stdio::piped())
                .spawn()
                .unwrap();
            let pid = Pid::from_u32(child.id().unwrap());
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            tokio::time::timeout(Duration::from_secs(10), async {
                while let Some(line) = lines.next_line().await.unwrap() {
                    if line.contains("MEMORY_FIXTURE_READY") {
                        return;
                    }
                }
                panic!("memory fixture did not start");
            })
            .await
            .unwrap();
            let mut observed = false;
            let result = sample_memory_with_reader(
                root,
                None,
                &AtomicBool::new(false),
                |queried, identity, rss| {
                    if queried != pid {
                        return resident_memory_with_fallback(
                            queried,
                            identity,
                            rss,
                            native_resident_memory,
                        );
                    }
                    observed = true;
                    // Force the fallback after discovery, then deterministically reap this child.
                    resident_memory_with_fallback(queried, identity, 0, |queried, identity| {
                        if !depart {
                            return Err("live process memory read denied");
                        }
                        child.start_kill().unwrap();
                        let deadline = std::time::Instant::now() + Duration::from_secs(10);
                        while child.try_wait().unwrap().is_none() {
                            assert!(std::time::Instant::now() < deadline, "fixture did not exit");
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        native_resident_memory(queried, identity)
                    })
                },
            );
            let _ = child.start_kill();
            child.wait().await.unwrap();
            assert!(observed, "descendant was not discovered");
            if depart {
                assert!(
                    result.is_ok(),
                    "departed child failed the sample: {result:?}"
                );
            } else {
                assert_eq!(result, Err("live process memory read denied"));
            }
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_native_response_requires_the_complete_record() {
        // SAFETY: proc_taskallinfo contains only integer fields and arrays of integers.
        let mut info: libc::proc_taskallinfo = unsafe { std::mem::zeroed() };
        info.pbsd.pbi_pid = 42;
        info.pbsd.pbi_ppid = 7;
        info.pbsd.pbi_start_tvsec = 100;
        let size = std::mem::size_of::<libc::proc_taskallinfo>() as libc::c_int;
        for returned in [-1, 0, size - 1] {
            assert!(macos_memory_response(&info, returned).is_err());
        }
        let memory = macos_memory_response(&info, size).unwrap();
        assert_eq!(memory.pid, Pid::from_u32(42));
        assert_eq!(memory.identity.parent, Some(Pid::from_u32(7)));
        assert_eq!(memory.identity.start_time, 100);
        assert_eq!(memory.rss_bytes, 0);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_departure_errors_do_not_hide_live_read_failures() {
        for code in [libc::ENOENT, libc::ESRCH] {
            assert!(linux_process_departed_error(&io::Error::from_raw_os_error(
                code
            )));
        }
        for code in [libc::EACCES, libc::EPERM, libc::EIO] {
            assert!(!linux_process_departed_error(
                &io::Error::from_raw_os_error(code)
            ));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_native_response_checks_complete_stat_and_memory_units() {
        let mut fields = vec!["0"; 50];
        fields[0] = "S";
        fields[1] = "7";
        fields[19] = "1099";
        let stat = format!("42 (a name ) with (parentheses)) {}\n", fields.join(" "));
        let memory = linux_memory_response(&stat, 4096, 100, 90).unwrap();
        assert_eq!(memory.pid, Pid::from_u32(42));
        assert_eq!(memory.identity.parent, Some(Pid::from_u32(7)));
        assert_eq!(memory.identity.start_time, 100);
        assert_eq!(memory.rss_bytes, 0);
        fields[21] = "2";
        let stat = format!("42 (name) {}\n", fields.join(" "));
        assert_eq!(
            linux_memory_response(&stat, 4096, 100, 90)
                .unwrap()
                .rss_bytes,
            8192
        );
        let short_stat = format!("42 (name) {}\n", fields[..22].join(" "));
        assert_eq!(
            linux_memory_response(&short_stat, 4096, 100, 90)
                .unwrap()
                .rss_bytes,
            8192
        );
        assert!(linux_memory_response(stat.trim_end(), 4096, 100, 90).is_err());
        assert!(linux_memory_response("42 (name) S 7\n", 4096, 100, 90).is_err());
        assert!(linux_memory_response(&stat, 4096, 0, 90).is_err());
        fields[21] = "-1";
        let stat = format!("42 (name) {}\n", fields.join(" "));
        assert!(linux_memory_response(&stat, 4096, 100, 90).is_err());
    }

    fn fixture_command(case: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
            .env(FIXTURE_ENV, case)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .stdin(Stdio::null());
        #[cfg(unix)]
        command.process_group(0);
        command
    }

    #[cfg(unix)]
    struct OwnedFixtureGroup(Option<u32>);

    #[cfg(unix)]
    impl Drop for OwnedFixtureGroup {
        fn drop(&mut self) {
            if let Some(pid) = self.0.take() {
                // SAFETY: this test owns the still-unreaped process-group leader.
                unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
            }
        }
    }

    async fn run_fixture(case: &str, memory_mb: u64) -> Result<ExitStatus, MemoryWaitError> {
        let _fixture_lock = FIXTURE_LOCK.lock().await;
        let mut child = ManagedChild::spawn(fixture_command(case), memory_mb).unwrap();
        #[cfg(unix)]
        let mut group = OwnedFixtureGroup(child.id());
        let stdout = child.stdout().take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                if line.contains("MEMORY_FIXTURE_READY") {
                    return;
                }
            }
            panic!("memory fixture did not start");
        })
        .await
        .expect("memory fixture startup timed out");
        let result =
            tokio::time::timeout(Duration::from_secs(10), child.wait_with_memory(memory_mb)).await;
        if matches!(result, Ok(Ok(_))) {
            #[cfg(unix)]
            {
                group.0 = None;
            }
        } else {
            #[cfg(unix)]
            drop(group);
            let _ = child.start_kill();
            tokio::time::timeout(Duration::from_secs(10), child.wait())
                .await
                .expect("owned memory fixture cleanup timed out")
                .unwrap();
        }
        result.expect("memory watchdog fixture timed out")
    }

    #[tokio::test]
    async fn touched_resident_allocation_exceeds_threshold() {
        let result = run_fixture("resident", 128).await;
        assert!(matches!(
            result,
            Err(MemoryWaitError::Exceeded {
                limit_mb: 128,
                rss_bytes,
            }) if rss_bytes > 128 * BYTES_PER_MB
        ));
    }

    #[tokio::test]
    async fn descendant_resident_allocation_counts_toward_threshold() {
        let result = run_fixture("descendant", 224).await;
        assert!(matches!(
            result,
            Err(MemoryWaitError::Exceeded {
                limit_mb: 224,
                rss_bytes,
            }) if rss_bytes > 224 * BYTES_PER_MB
        ));
    }

    #[cfg(all(unix, target_pointer_width = "64"))]
    #[tokio::test]
    async fn virtual_reservation_does_not_exceed_resident_threshold() {
        assert!(run_fixture("virtual", 128).await.unwrap().success());
    }

    #[tokio::test]
    async fn zero_threshold_preserves_ordinary_wait() {
        assert!(run_fixture("resident", 0).await.unwrap().success());
    }

    #[tokio::test]
    async fn ordinary_completion_under_budget() {
        assert!(run_fixture("ordinary", 128).await.unwrap().success());
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn root_exit_closes_owned_job_descendants() {
        use windows::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };

        let mut child = ManagedChild::spawn(fixture_command("root-first"), 512).unwrap();
        let mut lines = BufReader::new(child.stdout().take().unwrap()).lines();
        let pid = tokio::time::timeout(Duration::from_secs(10), async {
            while let Some(line) = lines.next_line().await.unwrap() {
                // libtest prefixes its first uncaptured line with the fixture's test name.
                if let Some((_, pid)) = line.split_once("MEMORY_FIXTURE_CHILD=") {
                    return pid.parse::<u32>().unwrap();
                }
            }
            panic!("owned descendant did not start");
        })
        .await
        .unwrap();
        // SAFETY: the PID came from the child this test spawned in its owned job.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }.unwrap();
        let result =
            tokio::time::timeout(Duration::from_secs(3), child.wait_with_memory(512)).await;
        drop(child);
        // SAFETY: wait on and close only the handle opened above.
        let exited = unsafe { WaitForSingleObject(handle, 5_000) };
        unsafe { CloseHandle(handle) }.unwrap();
        assert!(result.unwrap().unwrap().success());
        assert_eq!(
            exited, WAIT_OBJECT_0,
            "owned job descendant survived root exit"
        );
    }

    fn hold_resident_memory(memory_mb: usize) {
        let mut allocation = vec![0_u8; memory_mb * BYTES_PER_MB as usize];
        for page in allocation.chunks_mut(4096) {
            // SAFETY: every chunk is nonempty and belongs to the live allocation.
            unsafe { std::ptr::write_volatile(page.as_mut_ptr(), 1) };
        }
        println!("MEMORY_FIXTURE_READY");
        std::thread::sleep(Duration::from_secs(2));
        std::hint::black_box(allocation);
    }

    #[test]
    fn memory_fixture() {
        let Ok(case) = std::env::var(FIXTURE_ENV) else {
            return;
        };
        match case.as_str() {
            #[cfg(windows)]
            "root-first" => {
                let child = std::process::Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
                    .env(FIXTURE_ENV, "lingering")
                    .stdout(Stdio::null())
                    .spawn()
                    .unwrap();
                println!("MEMORY_FIXTURE_CHILD={}", child.id());
            }
            #[cfg(windows)]
            "lingering" => std::thread::sleep(Duration::from_secs(30)),
            "resident" => hold_resident_memory(256),
            "small-resident" => hold_resident_memory(128),
            "descendant" => {
                // Each descendant touches less than the aggregate threshold. The root
                // remains alive and small while both owned descendants hold their pages.
                let mut descendants = Vec::new();
                for _ in 0..2 {
                    descendants.push(
                        std::process::Command::new(std::env::current_exe().unwrap())
                            .args(["--exact", FIXTURE_TEST, "--nocapture", "--test-threads=1"])
                            .env(FIXTURE_ENV, "small-resident")
                            .spawn()
                            .unwrap(),
                    );
                }
                for mut descendant in descendants {
                    assert!(descendant.wait().unwrap().success());
                }
            }
            #[cfg(all(unix, target_pointer_width = "64"))]
            "virtual" => {
                let len = 2 * 1024 * BYTES_PER_MB as usize;
                // SAFETY: reserve anonymous inaccessible pages without touching them.
                let reservation = unsafe {
                    libc::mmap(
                        std::ptr::null_mut(),
                        len,
                        libc::PROT_NONE,
                        libc::MAP_PRIVATE | libc::MAP_ANON,
                        -1,
                        0,
                    )
                };
                assert_ne!(reservation, libc::MAP_FAILED);
                println!("MEMORY_FIXTURE_READY");
                std::thread::sleep(Duration::from_millis(300));
                // SAFETY: unmap exactly the range successfully reserved above.
                assert_eq!(unsafe { libc::munmap(reservation, len) }, 0);
            }
            "await-release" => {
                use std::io::{Read, Write};
                println!("MEMORY_FIXTURE_READY");
                std::io::stdout().flush().unwrap();
                // The parent holds stdin open until it explicitly kills and reaps this child.
                let mut release = [0];
                std::io::stdin().read_exact(&mut release).unwrap();
            }
            "ordinary" => {
                println!("MEMORY_FIXTURE_READY");
                std::thread::sleep(Duration::from_millis(300));
            }
            _ => panic!("unknown owned memory fixture"),
        }
    }
}
