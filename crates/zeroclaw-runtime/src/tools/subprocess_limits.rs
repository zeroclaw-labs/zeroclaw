use std::io;
use std::process::ExitStatus;

#[cfg(any(all(unix, not(target_os = "macos")), windows))]
const BYTES_PER_MIB: u64 = 1024 * 1024;

pub(crate) struct MemoryLimitGuard {
    #[cfg(windows)]
    job: Option<usize>,
}

impl MemoryLimitGuard {
    fn empty() -> Self {
        Self {
            #[cfg(windows)]
            job: None,
        }
    }
}

#[cfg(windows)]
impl Drop for MemoryLimitGuard {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(
                    windows::Win32::Foundation::HANDLE(job as _),
                );
            }
        }
    }
}

pub(crate) fn apply_pre_spawn_memory_limit(
    cmd: &mut tokio::process::Command,
    max_memory_mb: u64,
) -> io::Result<()> {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        use std::os::unix::process::CommandExt;

        let Some(limit) = memory_limit_bytes(max_memory_mb)? else {
            return Ok(());
        };
        #[cfg(any(target_os = "android", target_os = "linux"))]
        let limit: libc::rlim_t = limit;
        #[cfg(not(any(target_os = "android", target_os = "linux")))]
        let limit: libc::rlim_t = limit.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shell_max_memory_mb exceeds platform rlimit capacity",
            )
        })?;

        unsafe {
            cmd.as_std_mut().pre_exec(move || {
                let rlimit = libc::rlimit {
                    rlim_cur: limit,
                    rlim_max: limit,
                };
                if libc::setrlimit(libc::RLIMIT_AS as _, &rlimit) != 0 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }

    #[cfg(any(not(unix), target_os = "macos"))]
    {
        // macOS exposes address-space/data-size ulimit knobs, but setting them
        // currently fails with EINVAL. Do not fail every shell command on
        // platforms where the process-memory rlimit is unavailable.
        let _ = (cmd, max_memory_mb);
    }

    Ok(())
}

pub(crate) fn apply_post_spawn_memory_limit(
    child: &tokio::process::Child,
    max_memory_mb: u64,
) -> io::Result<MemoryLimitGuard> {
    #[cfg(windows)]
    {
        use std::os::windows::io::RawHandle;
        use windows::Win32::Foundation::HANDLE;
        use windows::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };

        let Some(limit) = memory_limit_bytes(max_memory_mb)? else {
            return Ok(MemoryLimitGuard::empty());
        };
        let limit: usize = limit.try_into().map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shell_max_memory_mb exceeds Windows Job Object capacity",
            )
        })?;
        let job = unsafe { CreateJobObjectW(None, None) }.map_err(windows_error)?;
        let guard = MemoryLimitGuard {
            job: Some(job.0 as usize),
        };
        let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY;
        info.ProcessMemoryLimit = limit;

        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
            .map_err(windows_error)?;
            let child_handle: RawHandle = child.raw_handle().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::Other,
                    "Windows child process handle is unavailable",
                )
            })?;
            AssignProcessToJobObject(job, HANDLE(child_handle as _)).map_err(windows_error)?;
        }
        return Ok(guard);
    }

    #[cfg(not(windows))]
    {
        let _ = child;
        let _ = max_memory_mb;
        Ok(MemoryLimitGuard::empty())
    }
}

pub(crate) fn memory_limit_exceeded_error(
    status: ExitStatus,
    stderr: &str,
    max_memory_mb: u64,
) -> Option<String> {
    if !memory_limit_is_enforced(max_memory_mb) {
        return None;
    }

    if process_status_indicates_memory_limit(status, stderr)
        || stderr_mentions_memory_exhaustion(stderr)
    {
        Some(memory_limit_error_message(max_memory_mb, stderr))
    } else {
        None
    }
}

fn memory_limit_error_message(max_memory_mb: u64, stderr: &str) -> String {
    const STDERR_DETAIL_LIMIT: usize = 4096;

    let mut message =
        format!("memory limit exceeded: subprocess exceeded shell_max_memory_mb={max_memory_mb}");
    let stderr = stderr.trim();
    if stderr.is_empty() {
        return message;
    }

    let mut end = STDERR_DETAIL_LIMIT.min(stderr.len());
    while end > 0 && !stderr.is_char_boundary(end) {
        end -= 1;
    }
    message.push_str("; stderr: ");
    message.push_str(&stderr[..end]);
    if end < stderr.len() {
        message.push_str("\n... [stderr detail truncated]");
    }
    message
}

fn memory_limit_is_enforced(max_memory_mb: u64) -> bool {
    if max_memory_mb == 0 {
        return false;
    }

    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    {
        true
    }

    #[cfg(not(any(all(unix, not(target_os = "macos")), windows)))]
    {
        false
    }
}

#[cfg(any(all(unix, not(target_os = "macos")), windows))]
fn memory_limit_bytes(max_memory_mb: u64) -> io::Result<Option<u64>> {
    if max_memory_mb == 0 {
        return Ok(None);
    }
    max_memory_mb
        .checked_mul(BYTES_PER_MIB)
        .map(Some)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "shell_max_memory_mb is too large to convert to bytes",
            )
        })
}

#[cfg(windows)]
fn windows_error(err: windows::core::Error) -> io::Error {
    io::Error::new(io::ErrorKind::Other, err)
}

fn stderr_mentions_memory_exhaustion(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    lower.contains("cannot allocate memory")
        || lower.contains("out of memory")
        || lower.contains("memory allocation")
        || lower.contains("not enough memory")
}

#[cfg(unix)]
fn process_status_indicates_memory_limit(_status: ExitStatus, _stderr: &str) -> bool {
    // Unix signal exits alone are ambiguous: SIGKILL, SIGABRT, SIGBUS, and
    // SIGSEGV can come from explicit kills, process bugs, or OS pressure that
    // is not the configured shell memory ceiling. Use stderr text for Unix
    // classification and reserve status-only rewrites for platform-specific
    // memory status codes such as Windows NTSTATUS values.
    false
}

#[cfg(windows)]
fn process_status_indicates_memory_limit(status: ExitStatus, _stderr: &str) -> bool {
    matches!(
        status.code().map(|code| code as u32),
        Some(0xC000_0017 | 0xC000_012D)
    )
}

#[cfg(not(any(unix, windows)))]
fn process_status_indicates_memory_limit(_status: ExitStatus, _stderr: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn failing_status() -> ExitStatus {
        use std::os::unix::process::ExitStatusExt;

        ExitStatus::from_raw(1 << 8)
    }

    #[cfg(windows)]
    fn failing_status() -> ExitStatus {
        std::process::Command::new("cmd")
            .args(["/C", "exit", "/B", "1"])
            .status()
            .expect("cmd should run")
    }

    #[cfg(not(any(unix, windows)))]
    fn failing_status() -> ExitStatus {
        std::process::Command::new("false")
            .status()
            .expect("false should run")
    }

    #[cfg(any(all(unix, not(target_os = "macos")), windows))]
    #[test]
    fn classifies_memory_exhaustion_stderr() {
        let error =
            memory_limit_exceeded_error(failing_status(), "fatal: cannot allocate memory", 64);

        assert_eq!(
            error.as_deref(),
            Some(
                "memory limit exceeded: subprocess exceeded shell_max_memory_mb=64; stderr: fatal: cannot allocate memory"
            )
        );
    }

    #[test]
    fn disabled_memory_limit_never_rewrites_error() {
        let error =
            memory_limit_exceeded_error(failing_status(), "fatal: cannot allocate memory", 0);

        assert!(error.is_none());
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unix_signal_without_memory_stderr_does_not_rewrite_error() {
        use std::os::unix::process::ExitStatusExt;

        let status = ExitStatus::from_raw(libc::SIGKILL);
        let error = memory_limit_exceeded_error(status, "", 64);

        assert!(error.is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_without_enforced_limit_never_rewrites_error() {
        let error =
            memory_limit_exceeded_error(failing_status(), "fatal: cannot allocate memory", 64);

        assert!(error.is_none());
    }
}
