use crate::db::pool::DbPool;
use crate::docker::DockerManager;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::broadcast;
use tokio::time::interval;
use tracing::{error, info, warn};

const CHECK_INTERVAL_SECS: u64 = 30;
const FAILURE_THRESHOLD: u32 = 3;
const RESTART_WAIT_SECS: u64 = 10;
const HEALTH_TIMEOUT_SECS: u64 = 5;

// ---------------------------------------------------------------------------
// FailureTracker
// ---------------------------------------------------------------------------

struct FailureTracker {
    counts: HashMap<String, u32>,
}

impl FailureTracker {
    fn new() -> Self {
        Self {
            counts: HashMap::new(),
        }
    }

    /// Increment consecutive failure count and return new count.
    fn record_failure(&mut self, tenant_id: &str) -> u32 {
        let count = self.counts.entry(tenant_id.to_string()).or_insert(0);
        *count += 1;
        *count
    }

    /// Clear failure count on success.
    fn record_success(&mut self, tenant_id: &str) {
        self.counts.remove(tenant_id);
    }
}

// ---------------------------------------------------------------------------
// Background loop
// ---------------------------------------------------------------------------

pub async fn run(db: DbPool, docker: DockerManager, mut shutdown_rx: broadcast::Receiver<()>) {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_secs(HEALTH_TIMEOUT_SECS))
        .build()
    {
        Ok(c) => c,
        Err(e) => {
            error!("health_checker: failed to build HTTP client: {}", e);
            return;
        }
    };

    let mut ticker = interval(Duration::from_secs(CHECK_INTERVAL_SECS));
    let mut tracker = FailureTracker::new();

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                check_all_tenants(&db, &docker, &client, &mut tracker).await;
            }
            _ = shutdown_rx.recv() => {
                info!("health_checker: shutdown signal received");
                break;
            }
        }
    }
}

/// Query all running tenants and check each one.
/// Also check 'error' and 'starting' tenants for recovery.
async fn check_all_tenants(
    db: &DbPool,
    docker: &DockerManager,
    client: &reqwest::Client,
    tracker: &mut FailureTracker,
) {
    // Check running tenants for failures
    let tenants: Vec<(String, String, u16, String)> = match db.read(|conn| {
        let mut stmt = conn.prepare(
            "SELECT id, slug, port, status FROM tenants WHERE status IN ('running', 'error', 'starting')",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)? as u16,
                    row.get::<_, String>(3)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }) {
        Ok(t) => t,
        Err(e) => {
            error!("health_checker: failed to load tenants: {}", e);
            return;
        }
    };

    tracing::debug!("Health checker: checking {} active tenants", tenants.len());

    for (tenant_id, slug, port, status) in tenants {
        if status == "running" {
            check_tenant(db, docker, client, tracker, &tenant_id, &slug, port).await;
        } else {
            // For error/starting tenants: check if they recovered
            recover_tenant(db, client, &tenant_id, &slug, port, &status).await;
        }
    }
}

/// Check a single tenant's health endpoint.
async fn check_tenant(
    db: &DbPool,
    docker: &DockerManager,
    client: &reqwest::Client,
    tracker: &mut FailureTracker,
    tenant_id: &str,
    slug: &str,
    port: u16,
) {
    let url = format!("http://127.0.0.1:{}/health", port);
    let healthy = client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if healthy {
        tracker.record_success(tenant_id);
        return;
    }

    let failures = tracker.record_failure(tenant_id);
    warn!(
        "health_checker: tenant {} (slug={}) health check failed ({}/{})",
        tenant_id, slug, failures, FAILURE_THRESHOLD
    );

    if failures >= FAILURE_THRESHOLD {
        attempt_restart(db, docker, client, tracker, tenant_id, slug, port).await;
    }
}

/// Restart the tenant container and re-check health.
async fn attempt_restart(
    db: &DbPool,
    docker: &DockerManager,
    client: &reqwest::Client,
    tracker: &mut FailureTracker,
    tenant_id: &str,
    slug: &str,
    port: u16,
) {
    warn!(
        "health_checker: restarting tenant {} (slug={}) after {} consecutive failures",
        tenant_id, slug, FAILURE_THRESHOLD
    );

    // Mark as 'starting' so other subsystems know a restart is in progress.
    if let Err(e) = db.write(|conn| {
        conn.execute(
            "UPDATE tenants SET status = 'starting' WHERE id = ?1",
            rusqlite::params![tenant_id],
        )?;
        Ok(())
    }) {
        error!(
            "health_checker: failed to set status=starting for {}: {}",
            tenant_id, e
        );
    }

    if let Err(e) = docker.restart_container(slug) {
        error!(
            "health_checker: restart failed for tenant {}: {}",
            tenant_id, e
        );
        set_status(db, tenant_id, "error");
        return;
    }

    // Wait for the container to initialise.
    tokio::time::sleep(Duration::from_secs(RESTART_WAIT_SECS)).await;

    let url = format!("http://127.0.0.1:{}/health", port);
    let healthy = client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    let new_status = if healthy { "running" } else { "error" };
    info!(
        "health_checker: tenant {} post-restart status={}",
        tenant_id, new_status
    );

    if healthy {
        // Clear the failure streak so we start fresh.
        tracker.record_success(tenant_id);
    }

    set_status(db, tenant_id, new_status);
}

/// Check if an error/starting tenant has actually recovered (container healthy).
/// Promotes status back to 'running' if the health endpoint responds.
async fn recover_tenant(
    db: &DbPool,
    client: &reqwest::Client,
    tenant_id: &str,
    slug: &str,
    port: u16,
    current_status: &str,
) {
    let url = format!("http://127.0.0.1:{}/health", port);
    let healthy = client
        .get(&url)
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);

    if healthy {
        info!(
            "health_checker: tenant {} (slug={}) recovered from '{}' → running",
            tenant_id, slug, current_status
        );
        set_status(db, tenant_id, "running");
    }
}

fn set_status(db: &DbPool, tenant_id: &str, status: &str) {
    if let Err(e) = db.write(|conn| {
        conn.execute(
            "UPDATE tenants SET status = ?1 WHERE id = ?2",
            rusqlite::params![status, tenant_id],
        )?;
        Ok(())
    }) {
        error!(
            "health_checker: failed to set status={} for {}: {}",
            status, tenant_id, e
        );
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_failure_tracker_increments() {
        let mut tracker = FailureTracker::new();
        assert_eq!(tracker.record_failure("tenant-a"), 1);
        assert_eq!(tracker.record_failure("tenant-a"), 2);
        assert_eq!(tracker.record_failure("tenant-a"), 3);
    }

    #[test]
    fn test_failure_tracker_resets_on_success() {
        let mut tracker = FailureTracker::new();
        tracker.record_failure("tenant-a");
        tracker.record_failure("tenant-a");
        tracker.record_success("tenant-a");
        // After reset, count starts from 0.
        assert_eq!(tracker.record_failure("tenant-a"), 1);
    }

    #[test]
    fn test_failure_tracker_threshold() {
        let mut tracker = FailureTracker::new();
        // First two failures are below threshold.
        assert!(tracker.record_failure("tenant-b") < FAILURE_THRESHOLD);
        assert!(tracker.record_failure("tenant-b") < FAILURE_THRESHOLD);
        // Third failure meets the threshold.
        let count = tracker.record_failure("tenant-b");
        assert_eq!(count, FAILURE_THRESHOLD);
    }

    #[test]
    fn test_failure_tracker_independent_tenants() {
        let mut tracker = FailureTracker::new();
        tracker.record_failure("tenant-a");
        tracker.record_failure("tenant-a");
        // tenant-b is independent.
        assert_eq!(tracker.record_failure("tenant-b"), 1);
        assert_eq!(tracker.record_failure("tenant-a"), 3);
    }

    #[test]
    fn test_failure_tracker_success_on_unknown_tenant_is_noop() {
        let mut tracker = FailureTracker::new();
        // Should not panic when called for a tenant with no failures.
        tracker.record_success("tenant-unknown");
        assert_eq!(tracker.record_failure("tenant-unknown"), 1);
    }
}
