//! Runtime-authority guard — decides whether THIS process may reclaim a task.

use super::task_registry::TaskRecord;

pub fn is_authoritative(rec: &TaskRecord) -> bool {
    is_authoritative_with_process_probe(rec, process_state)
}

/// Check the recorded owner independently of the current task row. Recovery uses
/// this when an intent outlives a stale owner claim or the task row has already
/// been removed.
pub(crate) fn is_authoritative_owner(owner_pid: u32, owner_boot_id: &str) -> bool {
    is_authoritative_owner_with_process_probe(owner_pid, owner_boot_id, process_state)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProcessState {
    SameProcessAlive,
    AbsentOrReused,
    Unknown,
}

fn is_authoritative_with_process_probe(
    rec: &TaskRecord,
    probe: impl Fn(u32, Option<u64>) -> ProcessState,
) -> bool {
    is_authoritative_owner_with_process_probe(rec.owner_pid, &rec.owner_boot_id, probe)
}

fn is_authoritative_owner_with_process_probe(
    owner_pid: u32,
    owner_boot_id: &str,
    probe: impl Fn(u32, Option<u64>) -> ProcessState,
) -> bool {
    if owner_pid == 0 {
        return false;
    }

    let expected_started_at = match parse_process_identity(owner_boot_id) {
        ProcessIdentity::Structured { pid, started_at } if pid == owner_pid => started_at,
        ProcessIdentity::Structured { .. } => return false,
        ProcessIdentity::Legacy => None,
        ProcessIdentity::Malformed => return false,
    };

    matches!(
        probe(owner_pid, expected_started_at),
        ProcessState::AbsentOrReused
    )
}

enum ProcessIdentity {
    Structured { pid: u32, started_at: Option<u64> },
    Legacy,
    Malformed,
}

fn parse_process_identity(boot_id: &str) -> ProcessIdentity {
    let mut parts = boot_id.split(':');
    if parts.next() != Some("zc-process-v1") {
        return ProcessIdentity::Legacy;
    }
    let (Some(pid), Some(started_at), Some(nonce), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return ProcessIdentity::Malformed;
    };
    let Ok(pid) = pid.parse() else {
        return ProcessIdentity::Malformed;
    };
    let started_at = if started_at == "unknown" {
        None
    } else {
        let Ok(started_at) = started_at.parse() else {
            return ProcessIdentity::Malformed;
        };
        Some(started_at)
    };
    // The nonce is persisted for uniqueness but is not observable through OS
    // process APIs. Authority therefore remains conservative when PID and the
    // OS start time identify the same second.
    if nonce.is_empty() {
        return ProcessIdentity::Malformed;
    }
    ProcessIdentity::Structured { pid, started_at }
}

fn process_state(pid: u32, expected_started_at: Option<u64>) -> ProcessState {
    if !sysinfo::IS_SUPPORTED_SYSTEM {
        return ProcessState::Unknown;
    }
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = sysinfo::System::new();
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        sysinfo::ProcessRefreshKind::nothing(),
    );
    let Some(process) = system.process(pid) else {
        return match pid_is_definitely_absent(pid.as_u32()) {
            Some(true) => ProcessState::AbsentOrReused,
            Some(false) | None => ProcessState::Unknown,
        };
    };
    if process.start_time() == 0 {
        return ProcessState::Unknown;
    }
    match expected_started_at {
        Some(started_at) if process.start_time() != started_at => ProcessState::AbsentOrReused,
        Some(_) | None => ProcessState::SameProcessAlive,
    }
}

#[cfg(unix)]
fn pid_is_definitely_absent(pid: u32) -> Option<bool> {
    // SAFETY: signal 0 performs an existence/permission check without sending a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return Some(false);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(true),
        Some(libc::EPERM) => Some(false),
        _ => None,
    }
}

#[cfg(windows)]
fn pid_is_definitely_absent(pid: u32) -> Option<bool> {
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, WaitForSingleObject,
    };

    match unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            false,
            pid,
        )
    } {
        Ok(handle) => {
            let wait = unsafe { WaitForSingleObject(handle, 0) };
            // SAFETY: `handle` was returned by `OpenProcess` in this function.
            let _ = unsafe { CloseHandle(handle) };
            if wait == WAIT_OBJECT_0 {
                Some(true)
            } else if wait == WAIT_TIMEOUT {
                Some(false)
            } else {
                None
            }
        }
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => Some(true),
        Err(error) if error.code() == ERROR_ACCESS_DENIED.to_hresult() => Some(false),
        Err(_) => None,
    }
}

#[cfg(not(any(unix, windows)))]
fn pid_is_definitely_absent(_pid: u32) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control_plane::task_registry::{TaskKind, TaskStatus};

    fn rec(owner_pid: u32, owner_boot_id: &str) -> TaskRecord {
        TaskRecord {
            id: "t".into(),
            kind: TaskKind::Delegate,
            agent: "main".into(),
            status: TaskStatus::Running,
            owner_pid,
            owner_boot_id: owner_boot_id.into(),
            heartbeat_at: None,
            depth: 0,
            parent_id: None,
            originator_route: None,
            delivered: false,
            idem_key: None,
            principal_id: None,
            started_at: "2026-06-18T00:00:00Z".into(),
            finished_at: None,
        }
    }

    #[test]
    fn dead_legacy_owner_is_reclaimable() {
        assert!(is_authoritative_with_process_probe(
            &rec(999_999, "boot-OLD"),
            |_, _| ProcessState::AbsentOrReused,
        ));
    }

    #[test]
    fn unstamped_owner_fails_closed() {
        assert!(!is_authoritative_with_process_probe(
            &rec(0, "boot-NOW"),
            |_, _| ProcessState::AbsentOrReused,
        ));
    }

    #[test]
    fn live_same_boot_pid_is_not_reclaimed() {
        // Our own live pid, same boot ⇒ must NOT be reclaimed.
        assert!(!is_authoritative_with_process_probe(
            &rec(42, "boot-NOW"),
            |_, _| ProcessState::SameProcessAlive,
        ));
    }

    #[test]
    fn unstamped_boot_id_with_live_pid_is_not_reclaimed() {
        // Review finding #7: a record written before its boot_id is stamped (empty) and
        // owned by a LIVE pid must NOT be reaped via the boot-mismatch path — fail closed.
        assert!(!is_authoritative_with_process_probe(
            &rec(42, ""),
            |_, _| ProcessState::SameProcessAlive,
        ));
    }

    #[test]
    fn unstamped_boot_id_reclaims_only_when_pid_liveness_says_dead() {
        assert!(!is_authoritative_with_process_probe(
            &rec(42, ""),
            |_, _| ProcessState::SameProcessAlive,
        ));
        assert!(is_authoritative_with_process_probe(&rec(42, ""), |_, _| {
            ProcessState::AbsentOrReused
        },));
    }

    #[test]
    fn live_structured_owner_is_not_reclaimed_by_another_boot() {
        let rec = rec(42, "zc-process-v1:42:123:owner");
        assert!(!is_authoritative_with_process_probe(
            &rec,
            |pid, started_at| {
                assert_eq!(pid, 42);
                assert_eq!(started_at, Some(123));
                ProcessState::SameProcessAlive
            },
        ));
    }

    #[test]
    fn same_second_pid_reuse_is_conservatively_treated_as_live() {
        let rec = rec(42, "zc-process-v1:42:123:owner");
        assert!(!is_authoritative_with_process_probe(
            &rec,
            |pid, started_at| {
                assert_eq!(pid, 42);
                assert_eq!(started_at, Some(123));
                // The OS probe exposes PID plus second-resolution start time, not
                // the random process nonce. Same-second reuse therefore fails closed.
                ProcessState::SameProcessAlive
            },
        ));
    }

    #[test]
    fn reused_pid_is_reclaimable_when_start_time_differs() {
        let rec = rec(42, "zc-process-v1:42:123:owner");
        assert!(is_authoritative_with_process_probe(&rec, |_, _| {
            ProcessState::AbsentOrReused
        },));
    }

    #[test]
    fn malformed_structured_identity_fails_closed() {
        let rec = rec(42, "zc-process-v1:42:not-a-time:owner");
        assert!(!is_authoritative_with_process_probe(&rec, |_, _| {
            ProcessState::AbsentOrReused
        },));
    }

    #[test]
    fn structured_identity_without_start_time_uses_proven_pid_state() {
        let rec = rec(42, "zc-process-v1:42:unknown:owner");
        assert!(is_authoritative_with_process_probe(
            &rec,
            |_, started_at| {
                assert_eq!(started_at, None);
                ProcessState::AbsentOrReused
            }
        ));
        assert!(!is_authoritative_with_process_probe(&rec, |_, _| {
            ProcessState::SameProcessAlive
        }));
        assert!(!is_authoritative_with_process_probe(&rec, |_, _| {
            ProcessState::Unknown
        }));
    }

    #[test]
    fn unknown_process_state_fails_closed() {
        assert!(!is_authoritative_with_process_probe(
            &rec(42, "boot-OLD"),
            |_, _| ProcessState::Unknown,
        ));
    }

    #[cfg(windows)]
    #[test]
    fn windows_process_exit_is_detected() {
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping -n 30 127.0.0.1 >NUL"])
            .spawn()
            .expect("spawn long-lived Windows child");
        let pid = child.id();
        let live = rec(pid, &format!("zc-process-v1:{pid}:unknown:test"));
        assert!(!is_authoritative(&live));

        child.kill().expect("terminate Windows child");
        child.wait().expect("reap Windows child");
        assert!(is_authoritative(&live));
    }
}
