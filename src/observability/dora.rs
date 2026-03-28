use std::collections::VecDeque;
use std::sync::RwLock;
use std::time::Duration;

use chrono::{DateTime, Utc};

/// Maximum deployment records kept in the ring buffer.
/// Covers ~90 days at ~11 deploys/day.
const MAX_RECORDS: usize = 1000;

/// Time window constants.
const WINDOW_7D: Duration = Duration::from_secs(7 * 24 * 3600);
const WINDOW_30D: Duration = Duration::from_secs(30 * 24 * 3600);
const WINDOW_90D: Duration = Duration::from_secs(90 * 24 * 3600);

// ── Record types ─────────────────────────────────────────────

/// A single deployment record stored in the ring buffer.
#[derive(Debug, Clone)]
struct DeploymentRecord {
    /// When the deployment completed (success or failure).
    timestamp: DateTime<Utc>,
    /// Whether the deployment succeeded.
    success: bool,
    /// Lead time: duration from commit to deploy completion (if known).
    lead_time: Option<Duration>,
}

/// A single recovery record.
#[derive(Debug, Clone)]
struct RecoveryRecord {
    timestamp: DateTime<Utc>,
    duration: Duration,
}

// ── Snapshot ─────────────────────────────────────────────────

/// Point-in-time snapshot of DORA metrics for a given time window.
#[derive(Debug, Clone)]
pub struct DoraSnapshot {
    /// Total deployments in the window.
    pub total_deployments: u64,
    /// Failed deployments in the window.
    pub failed_deployments: u64,
    /// Change failure rate (0.0..=1.0). `None` if no deployments.
    pub change_failure_rate: Option<f64>,
    /// Average lead time for changes. `None` if no lead times recorded.
    pub mean_lead_time: Option<Duration>,
    /// Mean time to recovery. `None` if no recoveries recorded.
    pub mttr: Option<Duration>,
    /// Window duration used for this snapshot.
    pub window: Duration,
}

// ── Internal state ───────────────────────────────────────────

#[derive(Debug, Default)]
struct CollectorState {
    deployments: VecDeque<DeploymentRecord>,
    recoveries: VecDeque<RecoveryRecord>,
}

// ── DoraCollector ────────────────────────────────────────────

/// Thread-safe DORA metrics collector.
///
/// Tracks deployment frequency, lead time for changes, change failure rate,
/// and mean time to recovery (MTTR). Supports time-windowed views at
/// 7-day, 30-day, and 90-day intervals.
pub struct DoraCollector {
    inner: RwLock<CollectorState>,
}

impl DoraCollector {
    /// Create an empty collector.
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(CollectorState::default()),
        }
    }

    /// Record a completed deployment (success or failure).
    ///
    /// `lead_time` is the duration from commit to deploy completion.
    pub fn record_deployment(&self, success: bool, lead_time: Option<Duration>) {
        let mut state = self.inner.write().expect("DORA lock poisoned");
        if state.deployments.len() >= MAX_RECORDS {
            state.deployments.pop_front();
        }
        state.deployments.push_back(DeploymentRecord {
            timestamp: Utc::now(),
            success,
            lead_time,
        });
    }

    /// Record a failed deployment. Convenience wrapper around `record_deployment`.
    pub fn record_failure(&self) {
        self.record_deployment(false, None);
    }

    /// Record a recovery from a failed deployment.
    pub fn record_recovery(&self, duration: Duration) {
        let mut state = self.inner.write().expect("DORA lock poisoned");
        if state.recoveries.len() >= MAX_RECORDS {
            state.recoveries.pop_front();
        }
        state.recoveries.push_back(RecoveryRecord {
            timestamp: Utc::now(),
            duration,
        });
    }

    /// Produce a snapshot of DORA metrics for a 7-day window.
    pub fn snapshot_7d(&self) -> DoraSnapshot {
        self.snapshot_window(WINDOW_7D)
    }

    /// Produce a snapshot of DORA metrics for a 30-day window.
    pub fn snapshot_30d(&self) -> DoraSnapshot {
        self.snapshot_window(WINDOW_30D)
    }

    /// Produce a snapshot of DORA metrics for a 90-day window.
    pub fn snapshot_90d(&self) -> DoraSnapshot {
        self.snapshot_window(WINDOW_90D)
    }

    /// Produce a snapshot of DORA metrics (default 30-day window).
    pub fn snapshot(&self) -> DoraSnapshot {
        self.snapshot_window(WINDOW_30D)
    }

    fn snapshot_window(&self, window: Duration) -> DoraSnapshot {
        let state = self.inner.read().expect("DORA lock poisoned");
        let cutoff =
            Utc::now() - chrono::Duration::from_std(window).unwrap_or(chrono::Duration::MAX);

        // Filter deployments within window
        let deploys_in_window: Vec<&DeploymentRecord> = state
            .deployments
            .iter()
            .filter(|d| d.timestamp >= cutoff)
            .collect();

        let total_deployments = deploys_in_window.len() as u64;
        let failed_deployments = deploys_in_window.iter().filter(|d| !d.success).count() as u64;

        let change_failure_rate = if total_deployments > 0 {
            Some(failed_deployments as f64 / total_deployments as f64)
        } else {
            None
        };

        // Mean lead time
        let lead_times: Vec<Duration> = deploys_in_window
            .iter()
            .filter_map(|d| d.lead_time)
            .collect();
        let mean_lead_time = if lead_times.is_empty() {
            None
        } else {
            let count = u32::try_from(lead_times.len()).unwrap_or(u32::MAX);
            let total: Duration = lead_times.iter().sum();
            Some(total / count)
        };

        // MTTR
        let recoveries_in_window: Vec<&RecoveryRecord> = state
            .recoveries
            .iter()
            .filter(|r| r.timestamp >= cutoff)
            .collect();
        let mttr = if recoveries_in_window.is_empty() {
            None
        } else {
            let count = u32::try_from(recoveries_in_window.len()).unwrap_or(u32::MAX);
            let total: Duration = recoveries_in_window.iter().map(|r| r.duration).sum();
            Some(total / count)
        };

        DoraSnapshot {
            total_deployments,
            failed_deployments,
            change_failure_rate,
            mean_lead_time,
            mttr,
            window,
        }
    }
}

impl Default for DoraCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_collector_returns_none_rates() {
        let c = DoraCollector::new();
        let snap = c.snapshot_30d();
        assert_eq!(snap.total_deployments, 0);
        assert_eq!(snap.failed_deployments, 0);
        assert!(snap.change_failure_rate.is_none());
        assert!(snap.mean_lead_time.is_none());
        assert!(snap.mttr.is_none());
    }

    #[test]
    fn deployment_frequency_counts() {
        let c = DoraCollector::new();
        c.record_deployment(true, None);
        c.record_deployment(true, None);
        c.record_deployment(false, None);

        let snap = c.snapshot_30d();
        assert_eq!(snap.total_deployments, 3);
        assert_eq!(snap.failed_deployments, 1);
    }

    #[test]
    fn change_failure_rate_calculation() {
        let c = DoraCollector::new();
        c.record_deployment(true, None);
        c.record_deployment(false, None);
        c.record_deployment(true, None);
        c.record_deployment(false, None);

        let snap = c.snapshot_30d();
        let rate = snap.change_failure_rate.unwrap();
        assert!((rate - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn change_failure_rate_zero_failures() {
        let c = DoraCollector::new();
        c.record_deployment(true, None);
        c.record_deployment(true, None);

        let snap = c.snapshot_30d();
        let rate = snap.change_failure_rate.unwrap();
        assert!((rate - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn change_failure_rate_all_failures() {
        let c = DoraCollector::new();
        c.record_deployment(false, None);
        c.record_deployment(false, None);

        let snap = c.snapshot_30d();
        let rate = snap.change_failure_rate.unwrap();
        assert!((rate - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn lead_time_calculation() {
        let c = DoraCollector::new();
        c.record_deployment(true, Some(Duration::from_secs(100)));
        c.record_deployment(true, Some(Duration::from_secs(200)));
        c.record_deployment(true, Some(Duration::from_secs(300)));

        let snap = c.snapshot_30d();
        let mean = snap.mean_lead_time.unwrap();
        assert_eq!(mean, Duration::from_secs(200));
    }

    #[test]
    fn lead_time_ignores_none_entries() {
        let c = DoraCollector::new();
        c.record_deployment(true, Some(Duration::from_secs(100)));
        c.record_deployment(true, None); // no lead time
        c.record_deployment(true, Some(Duration::from_secs(300)));

        let snap = c.snapshot_30d();
        let mean = snap.mean_lead_time.unwrap();
        assert_eq!(mean, Duration::from_secs(200));
    }

    #[test]
    fn mttr_calculation() {
        let c = DoraCollector::new();
        c.record_recovery(Duration::from_secs(60));
        c.record_recovery(Duration::from_secs(120));
        c.record_recovery(Duration::from_secs(180));

        let snap = c.snapshot_30d();
        let mttr = snap.mttr.unwrap();
        assert_eq!(mttr, Duration::from_secs(120));
    }

    #[test]
    fn mttr_none_when_no_recoveries() {
        let c = DoraCollector::new();
        c.record_deployment(false, None);

        let snap = c.snapshot_30d();
        assert!(snap.mttr.is_none());
    }

    #[test]
    fn record_failure_convenience() {
        let c = DoraCollector::new();
        c.record_failure();

        let snap = c.snapshot_30d();
        assert_eq!(snap.total_deployments, 1);
        assert_eq!(snap.failed_deployments, 1);
        assert!((snap.change_failure_rate.unwrap() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn rapid_deployments() {
        let c = DoraCollector::new();
        for _ in 0..100 {
            c.record_deployment(true, Some(Duration::from_millis(50)));
        }

        let snap = c.snapshot_30d();
        assert_eq!(snap.total_deployments, 100);
        assert_eq!(snap.mean_lead_time.unwrap(), Duration::from_millis(50));
    }

    #[test]
    fn ring_buffer_eviction() {
        let c = DoraCollector::new();
        // Fill beyond MAX_RECORDS
        for i in 0..(MAX_RECORDS + 50) {
            c.record_deployment(i % 2 == 0, Some(Duration::from_secs(i as u64)));
        }

        let state = c.inner.read().unwrap();
        assert_eq!(state.deployments.len(), MAX_RECORDS);
    }

    #[test]
    fn recovery_ring_buffer_eviction() {
        let c = DoraCollector::new();
        for i in 0..(MAX_RECORDS + 50) {
            c.record_recovery(Duration::from_secs(i as u64));
        }

        let state = c.inner.read().unwrap();
        assert_eq!(state.recoveries.len(), MAX_RECORDS);
    }

    #[test]
    fn different_windows_return_correct_window_duration() {
        let c = DoraCollector::new();
        c.record_deployment(true, None);

        assert_eq!(c.snapshot_7d().window, WINDOW_7D);
        assert_eq!(c.snapshot_30d().window, WINDOW_30D);
        assert_eq!(c.snapshot_90d().window, WINDOW_90D);
    }

    #[test]
    fn default_impl_works() {
        let c = DoraCollector::default();
        let snap = c.snapshot();
        assert_eq!(snap.total_deployments, 0);
    }

    #[test]
    fn thread_safety_basic() {
        use std::sync::Arc;
        use std::thread;

        let c = Arc::new(DoraCollector::new());
        let mut handles = vec![];

        for _ in 0..4 {
            let c = Arc::clone(&c);
            handles.push(thread::spawn(move || {
                for _ in 0..25 {
                    c.record_deployment(true, Some(Duration::from_secs(10)));
                }
            }));
        }

        for h in handles {
            h.join().unwrap();
        }

        let snap = c.snapshot_30d();
        assert_eq!(snap.total_deployments, 100);
    }
}
