//! Runtime-authority guard — decides whether THIS process may reclaim a task.

use super::task_registry::TaskRecord;

pub fn is_authoritative(rec: &TaskRecord, current_boot_id: &str) -> bool {
    is_authoritative_with_pid_liveness(rec, current_boot_id, pid_is_alive)
}

fn is_authoritative_with_pid_liveness(
    rec: &TaskRecord,
    current_boot_id: &str,
    pid_is_alive: impl Fn(u32) -> bool,
) -> bool {
    // Fail-closed: an UNSTAMPED record (empty owner_boot_id) is never reclaimed by the
    // boot-mismatch path — otherwise a live task mid-create (record written before the
    // boot id is stamped) would be reaped by its own daemon. It is only reclaimable if
    // its owner pid is provably dead (handled below).
    if !rec.owner_boot_id.is_empty() && rec.owner_boot_id != current_boot_id {
        // Different, non-empty boot id ⇒ prior-boot orphan ⇒ safe to reclaim.
        return true;
    }
    // Same boot (or unstamped): only reclaim if the owning process is actually gone.
    !pid_is_alive(rec.owner_pid)
}

/// Best-effort liveness check for `pid`. On Linux we consult `/proc/<pid>`; on other
/// platforms we conservatively assume the process is alive (never reclaim a
/// same-boot task we cannot prove is dead).
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        // Unset owner — treat as not-alive so an un-stamped record is reclaimable.
        return false;
    }
    #[cfg(target_os = "linux")]
    {
        std::path::Path::new(&format!("/proc/{pid}")).exists()
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        true // conservative: do not reclaim what we cannot prove dead
    }
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
    fn prior_boot_is_reclaimable() {
        // Different boot id ⇒ orphan ⇒ authoritative regardless of pid.
        assert!(is_authoritative(&rec(999_999, "boot-OLD"), "boot-NEW"));
    }

    #[test]
    fn unstamped_owner_is_reclaimable() {
        // Same boot but pid 0 (never stamped) ⇒ reclaimable.
        assert!(is_authoritative(&rec(0, "boot-NOW"), "boot-NOW"));
    }

    #[test]
    fn live_same_boot_pid_is_not_reclaimed() {
        // Our own live pid, same boot ⇒ must NOT be reclaimed.
        let me = std::process::id();
        assert!(!is_authoritative(&rec(me, "boot-NOW"), "boot-NOW"));
    }

    #[test]
    fn unstamped_boot_id_with_live_pid_is_not_reclaimed() {
        // Review finding #7: a record written before its boot_id is stamped (empty) and
        // owned by a LIVE pid must NOT be reaped via the boot-mismatch path — fail closed.
        let me = std::process::id();
        assert!(!is_authoritative(&rec(me, ""), "boot-NEW"));
    }

    #[test]
    fn unstamped_boot_id_reclaims_only_when_pid_liveness_says_dead() {
        assert!(!is_authoritative_with_pid_liveness(
            &rec(42, ""),
            "boot-NEW",
            |_| true,
        ));
        assert!(is_authoritative_with_pid_liveness(
            &rec(42, ""),
            "boot-NEW",
            |_| false,
        ));
    }
}
