use std::path::Path;
use std::sync::Arc;
use std::path::PathBuf;

use tokio::sync::watch;

use crate::db::Registry;
use crate::lifecycle;

/// Interval between supervisor ticks.
const SUPERVISOR_INTERVAL_SECS: u64 = 15;

/// Run startup reconciliation: scan all instances and correct any
/// discrepancies between DB status and actual process state.
///
/// Called once at boot before accepting HTTP requests.
pub fn startup_reconcile(db_path: &Path) {
    let registry = match Registry::open(db_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Supervisor startup_reconcile: failed to open registry: {e:#}");
            return;
        }
    };

    let instances = match registry.list_instances() {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("Supervisor startup_reconcile: failed to list instances: {e:#}");
            return;
        }
    };

    for instance in &instances {
        let inst_dir = lifecycle::instance_dir_from(instance);

        // Try to acquire lifecycle lock (non-blocking). Skip if contended,
        // but log real errors (permission, corruption).
        let _lock = match lifecycle::try_lifecycle_lock(&inst_dir) {
            Ok(lifecycle::LockOutcome::Acquired(lock)) => lock,
            Ok(lifecycle::LockOutcome::Contended) => {
                tracing::info!(
                    "Supervisor: skipping '{}' (lifecycle lock held)",
                    instance.name
                );
                continue;
            }
            Err(e) => {
                tracing::error!(
                    "Supervisor: failed to open lifecycle lock for '{}': {e:#}",
                    instance.name
                );
                continue;
            }
        };

        reconcile_instance(&registry, instance, &inst_dir);
    }
}

/// Check a single instance and correct discrepancies.
fn reconcile_instance(registry: &Registry, instance: &crate::db::Instance, inst_dir: &Path) {
    let (live, live_pid) = match lifecycle::live_status(inst_dir) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(
                "Supervisor: failed to check live_status for '{}': {e:#}",
                instance.name
            );
            return;
        }
    };

    let is_alive = live == "running";
    let db_running = instance.status == "running";

    match (db_running, is_alive) {
        (true, false) => {
            // DB says running, but process is dead. Correct to stopped.
            tracing::warn!(
                "Supervisor: instance '{}' DB=running but process is dead. Marking stopped.",
                instance.name
            );
            if let Err(e) = registry.update_status(&instance.id, "stopped") {
                tracing::warn!(
                    "Supervisor: failed to update status for '{}': {e:#}",
                    instance.name
                );
            }
            // Clear DB PID cache (best-effort)
            if let Err(e) = registry.update_pid(&instance.id, None) {
                tracing::warn!(
                    "Supervisor: failed to clear PID for '{}': {e:#}",
                    instance.name
                );
            }
            // Remove stale pidfile
            if let Err(e) = lifecycle::remove_pid(inst_dir) {
                tracing::warn!(
                    "Supervisor: failed to remove stale pidfile for '{}': {e:#}",
                    instance.name
                );
            }
        }
        (false, true) => {
            // DB says stopped, but process is alive. Correct to running.
            tracing::info!(
                "Supervisor: instance '{}' DB=stopped but process is alive. Marking running.",
                instance.name
            );
            if let Err(e) = registry.update_status(&instance.id, "running") {
                tracing::warn!(
                    "Supervisor: failed to update status for '{}': {e:#}",
                    instance.name
                );
            }
            // Set DB PID cache (best-effort)
            if let Some(pid) = live_pid {
                if let Err(e) = registry.update_pid(&instance.id, Some(pid)) {
                    tracing::warn!(
                        "Supervisor: failed to set PID for '{}': {e:#}",
                        instance.name
                    );
                }
            }
        }
        (true, true) => {
            // Both agree it's running. Sync PID cache if mismatched.
            if let Some(pid) = live_pid {
                if instance.pid != Some(pid) {
                    if let Err(e) = registry.update_pid(&instance.id, Some(pid)) {
                        tracing::warn!(
                            "Supervisor: failed to sync PID for '{}': {e:#}",
                            instance.name
                        );
                    }
                }
            }
        }
        (false, false) => {
            // Both agree it's stopped. Nothing to do.
        }
    }
}

/// Run the periodic supervisor loop. Ticks every 15 seconds, checking
/// all instances for status drift.
///
/// Exits when the shutdown signal is received.
pub async fn run_supervisor(db_path: Arc<PathBuf>, mut shutdown: watch::Receiver<bool>) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(SUPERVISOR_INTERVAL_SECS));
    // First tick fires immediately; skip it since startup_reconcile already ran.
    interval.tick().await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                let db_path = db_path.clone();
                // Run check in spawn_blocking since it does sync I/O
                let _ = tokio::task::spawn_blocking(move || {
                    check_all_instances(&db_path);
                }).await;
            }
            _ = shutdown.changed() => {
                tracing::info!("Supervisor shutting down");
                return;
            }
        }
    }
}

/// Single tick: open registry, scan all instances, reconcile.
pub fn check_all_instances(db_path: &Path) {
    let registry = match Registry::open(db_path) {
        Ok(r) => r,
        Err(e) => {
            tracing::error!("Supervisor: failed to open registry: {e:#}");
            return;
        }
    };

    let instances = match registry.list_instances() {
        Ok(i) => i,
        Err(e) => {
            tracing::error!("Supervisor: failed to list instances: {e:#}");
            return;
        }
    };

    for instance in &instances {
        let inst_dir = lifecycle::instance_dir_from(instance);

        // Non-blocking lock; skip if contended, log real errors
        let _lock = match lifecycle::try_lifecycle_lock(&inst_dir) {
            Ok(lifecycle::LockOutcome::Acquired(lock)) => lock,
            Ok(lifecycle::LockOutcome::Contended) => {
                tracing::debug!(
                    "Supervisor: skipping '{}' (lifecycle lock held)",
                    instance.name
                );
                continue;
            }
            Err(e) => {
                tracing::error!(
                    "Supervisor: failed to open lifecycle lock for '{}': {e:#}",
                    instance.name
                );
                continue;
            }
        };

        reconcile_instance(&registry, instance, &inst_dir);
    }
}
