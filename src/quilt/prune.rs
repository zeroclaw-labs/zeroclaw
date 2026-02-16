use std::sync::atomic::{AtomicI64, Ordering};

use tracing::{debug, info, warn};

use super::client::{QuiltClient, QuiltContainerState, QuiltContainerStatus};

// ── Constants ───────────────────────────────────────────────────────

/// Minimum interval between prune runs (5 minutes).
const PRUNE_INTERVAL_MS: i64 = 5 * 60 * 1000;

/// Label key used to identify sandbox containers managed by Aria.
const LABEL_SANDBOX: &str = "aria.sandbox";

/// Label key for the container creation timestamp.
const LABEL_CREATED_AT_MS: &str = "aria.created_at_ms";

// ── Rate limiter ────────────────────────────────────────────────────

/// Tracks the last time a prune was executed (epoch ms).
static LAST_PRUNE_MS: AtomicI64 = AtomicI64::new(0);

/// Returns the current time in epoch milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Returns `true` if enough time has passed since the last prune.
/// If it returns `true`, it also updates the last-prune timestamp.
fn should_prune() -> bool {
    let now = now_ms();
    let last = LAST_PRUNE_MS.load(Ordering::Relaxed);

    if now - last < PRUNE_INTERVAL_MS {
        return false;
    }

    // CAS to prevent concurrent prune runs from racing
    LAST_PRUNE_MS
        .compare_exchange(last, now, Ordering::SeqCst, Ordering::Relaxed)
        .is_ok()
}

/// Reset the prune timer (for tests).
#[cfg(test)]
fn reset_prune_timer() {
    LAST_PRUNE_MS.store(0, Ordering::SeqCst);
}

// ── Filtering helpers ───────────────────────────────────────────────

/// Returns `true` if the container has the `aria.sandbox = "true"` label.
pub fn is_sandbox_container(status: &QuiltContainerStatus) -> bool {
    status
        .labels
        .as_ref()
        .and_then(|l| l.get(LABEL_SANDBOX))
        .is_some_and(|v| v == "true")
}

/// Extract the creation timestamp from labels, if present.
fn created_at_ms(status: &QuiltContainerStatus) -> Option<i64> {
    status
        .labels
        .as_ref()
        .and_then(|l| l.get(LABEL_CREATED_AT_MS))
        .and_then(|v| v.parse::<i64>().ok())
}

/// Returns `true` if the container has been idle longer than `idle_hours`.
///
/// A container is considered idle if:
/// - It has exited and `exited_at_ms` is older than `idle_hours`
/// - It has no `started_at_ms` and was created more than `idle_hours` ago
pub fn is_idle(status: &QuiltContainerStatus, idle_hours: u64) -> bool {
    let threshold_ms = now_ms() - (idle_hours as i64 * 3600 * 1000);

    match status.state {
        QuiltContainerState::Exited | QuiltContainerState::Error => {
            // Use exited_at_ms if available, otherwise created_at_ms
            if let Some(exited) = status.exited_at_ms {
                return exited < threshold_ms;
            }
            if let Some(created) = created_at_ms(status) {
                return created < threshold_ms;
            }
            // No timestamp info -- consider it idle
            true
        }
        QuiltContainerState::Pending => {
            // Pending containers that were created long ago are likely stuck
            if let Some(created) = created_at_ms(status) {
                return created < threshold_ms;
            }
            false
        }
        _ => false, // Running / Starting containers are not idle
    }
}

/// Returns `true` if the container is older than `max_age_days`, regardless of
/// state. This is a hard age limit to prevent zombie containers.
pub fn is_too_old(status: &QuiltContainerStatus, max_age_days: u64) -> bool {
    let threshold_ms = now_ms() - (max_age_days as i64 * 24 * 3600 * 1000);

    if let Some(created) = created_at_ms(status) {
        return created < threshold_ms;
    }

    // Fall back to started_at_ms
    if let Some(started) = status.started_at_ms {
        return started < threshold_ms;
    }

    false
}

// ── Prune ───────────────────────────────────────────────────────────

/// Prune sandbox containers that are idle or too old.
///
/// - `idle_hours`: delete exited/errored containers that have been idle for
///   this many hours.
/// - `max_age_days`: delete any sandbox container older than this, regardless
///   of state.
///
/// This function is rate-limited to run at most once every 5 minutes.
/// Returns the number of containers deleted, or 0 if skipped due to rate limit.
pub async fn prune_sandbox_containers(
    client: &QuiltClient,
    idle_hours: u64,
    max_age_days: u64,
) -> Result<usize, anyhow::Error> {
    if !should_prune() {
        debug!("Prune skipped (rate-limited to every 5 minutes)");
        return Ok(0);
    }

    info!(
        "Starting sandbox container prune (idle_hours={idle_hours}, max_age_days={max_age_days})"
    );

    let containers = client.list_containers().await?;
    let mut deleted = 0;

    for container in &containers {
        // Only touch sandbox containers
        if !is_sandbox_container(container) {
            continue;
        }

        let should_delete = is_too_old(container, max_age_days) || is_idle(container, idle_hours);

        if should_delete {
            info!(
                container_id = %container.id,
                name = %container.name,
                state = %container.state,
                "Pruning sandbox container"
            );

            // Stop running containers before deleting
            if container.state == QuiltContainerState::Running
                || container.state == QuiltContainerState::Starting
            {
                if let Err(e) = client.stop_container(&container.id).await {
                    warn!(error = %e, container_id = %container.id, "Failed to stop container before pruning");
                }
            }

            match client.delete_container(&container.id).await {
                Ok(()) => {
                    deleted += 1;
                    debug!(container_id = %container.id, "Container deleted");
                }
                Err(e) => {
                    warn!(error = %e, container_id = %container.id, "Failed to delete container during prune");
                }
            }
        }
    }

    info!("Prune complete: {deleted} container(s) deleted");
    Ok(deleted)
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_container(
        id: &str,
        state: QuiltContainerState,
        labels: Option<HashMap<String, String>>,
        started_at_ms: Option<i64>,
        exited_at_ms: Option<i64>,
    ) -> QuiltContainerStatus {
        QuiltContainerStatus {
            id: id.into(),
            tenant_id: None,
            name: format!("sandbox-{id}"),
            state,
            pid: None,
            exit_code: None,
            ip_address: None,
            created_at: None,
            memory_limit_mb: None,
            cpu_limit_percent: None,
            labels,
            started_at_ms,
            exited_at_ms,
        }
    }

    fn sandbox_labels(created_ms: i64) -> HashMap<String, String> {
        HashMap::from([
            (LABEL_SANDBOX.into(), "true".into()),
            (LABEL_CREATED_AT_MS.into(), created_ms.to_string()),
        ])
    }

    fn non_sandbox_labels() -> HashMap<String, String> {
        HashMap::from([("other.label".into(), "value".into())])
    }

    // ── is_sandbox_container ────────────────────────────────────

    #[test]
    fn sandbox_label_present() {
        let c = make_container(
            "s1",
            QuiltContainerState::Running,
            Some(sandbox_labels(now_ms())),
            Some(now_ms()),
            None,
        );
        assert!(is_sandbox_container(&c));
    }

    #[test]
    fn non_sandbox_label() {
        let c = make_container(
            "ns1",
            QuiltContainerState::Running,
            Some(non_sandbox_labels()),
            Some(now_ms()),
            None,
        );
        assert!(!is_sandbox_container(&c));
    }

    #[test]
    fn no_labels_not_sandbox() {
        let c = make_container(
            "nl1",
            QuiltContainerState::Running,
            None,
            Some(now_ms()),
            None,
        );
        assert!(!is_sandbox_container(&c));
    }

    #[test]
    fn sandbox_label_false_not_sandbox() {
        let labels = HashMap::from([(LABEL_SANDBOX.into(), "false".into())]);
        let c = make_container(
            "sf1",
            QuiltContainerState::Running,
            Some(labels),
            Some(now_ms()),
            None,
        );
        assert!(!is_sandbox_container(&c));
    }

    // ── is_idle ─────────────────────────────────────────────────

    #[test]
    fn exited_container_idle_after_hours() {
        let three_hours_ago = now_ms() - (3 * 3600 * 1000);
        let c = make_container(
            "idle1",
            QuiltContainerState::Exited,
            Some(sandbox_labels(now_ms() - (24 * 3600 * 1000))),
            Some(now_ms() - (24 * 3600 * 1000)),
            Some(three_hours_ago),
        );
        // idle_hours = 2 -> container exited 3 hours ago -> idle
        assert!(is_idle(&c, 2));
        // idle_hours = 4 -> container exited 3 hours ago -> not idle
        assert!(!is_idle(&c, 4));
    }

    #[test]
    fn running_container_not_idle() {
        let c = make_container(
            "run1",
            QuiltContainerState::Running,
            Some(sandbox_labels(now_ms() - (48 * 3600 * 1000))),
            Some(now_ms() - (48 * 3600 * 1000)),
            None,
        );
        assert!(!is_idle(&c, 1));
    }

    #[test]
    fn error_container_idle() {
        let old_time = now_ms() - (10 * 3600 * 1000);
        let c = make_container(
            "err1",
            QuiltContainerState::Error,
            Some(sandbox_labels(old_time)),
            Some(old_time),
            Some(old_time),
        );
        assert!(is_idle(&c, 1));
    }

    #[test]
    fn pending_container_idle_if_old_enough() {
        let old_time = now_ms() - (5 * 3600 * 1000);
        let c = make_container(
            "pend1",
            QuiltContainerState::Pending,
            Some(sandbox_labels(old_time)),
            None,
            None,
        );
        assert!(is_idle(&c, 4));
        assert!(!is_idle(&c, 6));
    }

    #[test]
    fn starting_container_not_idle() {
        let c = make_container(
            "start1",
            QuiltContainerState::Starting,
            Some(sandbox_labels(now_ms() - (10 * 3600 * 1000))),
            Some(now_ms() - (10 * 3600 * 1000)),
            None,
        );
        assert!(!is_idle(&c, 1));
    }

    // ── is_too_old ──────────────────────────────────────────────

    #[test]
    fn container_too_old_by_creation_label() {
        let ten_days_ago = now_ms() - (10 * 24 * 3600 * 1000);
        let c = make_container(
            "old1",
            QuiltContainerState::Running,
            Some(sandbox_labels(ten_days_ago)),
            Some(ten_days_ago),
            None,
        );
        assert!(is_too_old(&c, 7));
        assert!(!is_too_old(&c, 14));
    }

    #[test]
    fn container_not_too_old() {
        let one_day_ago = now_ms() - (1 * 24 * 3600 * 1000);
        let c = make_container(
            "new1",
            QuiltContainerState::Running,
            Some(sandbox_labels(one_day_ago)),
            Some(one_day_ago),
            None,
        );
        assert!(!is_too_old(&c, 7));
    }

    #[test]
    fn container_too_old_by_started_at() {
        let twenty_days_ago = now_ms() - (20 * 24 * 3600 * 1000);
        let c = make_container(
            "old2",
            QuiltContainerState::Running,
            None, // no labels -> falls back to started_at_ms
            Some(twenty_days_ago),
            None,
        );
        assert!(is_too_old(&c, 14));
    }

    #[test]
    fn container_no_timestamps_not_too_old() {
        let c = make_container("nots", QuiltContainerState::Running, None, None, None);
        assert!(!is_too_old(&c, 7));
    }

    // ── Rate limiter ────────────────────────────────────────────

    #[test]
    fn should_prune_respects_interval() {
        // These tests run in parallel and share a global atomic. Be robust to
        // another test thread winning the CAS between our reset and call.
        let mut first_ok = false;
        for _ in 0..50 {
            reset_prune_timer();
            if should_prune() {
                first_ok = true;
                break;
            }
        }
        assert!(first_ok);

        // Immediate second call should be rate-limited
        assert!(!should_prune());

        // Simulate time passing by resetting the timer
        reset_prune_timer();
        let mut second_ok = false;
        for _ in 0..50 {
            reset_prune_timer();
            if should_prune() {
                second_ok = true;
                break;
            }
        }
        assert!(second_ok);
    }

    #[test]
    fn should_prune_allows_after_interval() {
        reset_prune_timer();

        // Simulate a prune that happened long ago
        let old_time = now_ms() - PRUNE_INTERVAL_MS - 1000;
        LAST_PRUNE_MS.store(old_time, Ordering::SeqCst);

        assert!(should_prune());
    }

    // ── Filtering combinations ──────────────────────────────────

    #[test]
    fn filter_mixed_containers() {
        let now = now_ms();
        let containers = vec![
            // Sandbox, running, fresh -> keep
            make_container(
                "keep1",
                QuiltContainerState::Running,
                Some(sandbox_labels(now - 1000)),
                Some(now - 1000),
                None,
            ),
            // Sandbox, exited 10 hours ago -> prune (idle_hours=2)
            make_container(
                "prune1",
                QuiltContainerState::Exited,
                Some(sandbox_labels(now - (24 * 3600 * 1000))),
                Some(now - (24 * 3600 * 1000)),
                Some(now - (10 * 3600 * 1000)),
            ),
            // Not a sandbox, exited -> keep (not managed by us)
            make_container(
                "keep2",
                QuiltContainerState::Exited,
                Some(non_sandbox_labels()),
                None,
                Some(now - (10 * 3600 * 1000)),
            ),
            // Sandbox, running, 30 days old -> prune (max_age=14)
            make_container(
                "prune2",
                QuiltContainerState::Running,
                Some(sandbox_labels(now - (30 * 24 * 3600 * 1000))),
                Some(now - (30 * 24 * 3600 * 1000)),
                None,
            ),
            // No labels -> keep
            make_container("keep3", QuiltContainerState::Running, None, Some(now), None),
        ];

        let idle_hours = 2;
        let max_age_days = 14;

        let to_prune: Vec<_> = containers
            .iter()
            .filter(|c| {
                is_sandbox_container(c) && (is_idle(c, idle_hours) || is_too_old(c, max_age_days))
            })
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(to_prune, vec!["prune1", "prune2"]);
    }

    #[test]
    fn filter_all_sandbox_containers() {
        let now = now_ms();
        let containers = vec![
            make_container(
                "s1",
                QuiltContainerState::Running,
                Some(sandbox_labels(now)),
                Some(now),
                None,
            ),
            make_container(
                "s2",
                QuiltContainerState::Exited,
                Some(sandbox_labels(now)),
                Some(now),
                Some(now),
            ),
            make_container("ns1", QuiltContainerState::Running, None, Some(now), None),
            make_container(
                "ns2",
                QuiltContainerState::Running,
                Some(non_sandbox_labels()),
                Some(now),
                None,
            ),
        ];

        let sandbox: Vec<_> = containers
            .iter()
            .filter(|c| is_sandbox_container(c))
            .map(|c| c.id.as_str())
            .collect();

        assert_eq!(sandbox, vec!["s1", "s2"]);
    }

    // ── Constants ───────────────────────────────────────────────

    #[test]
    fn prune_interval_is_five_minutes() {
        assert_eq!(PRUNE_INTERVAL_MS, 300_000);
    }
}
