//! Backpressure handling with queue depth tracking and adaptive load shedding.
//!
//! Provides priority-based admission control. When the queue is near capacity,
//! lower-priority requests are shed first.

use std::sync::atomic::{AtomicU64, Ordering};

/// Priority levels for incoming work items, from highest to lowest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    /// System-critical work that must not be shed (e.g., shutdown hooks).
    Critical = 4,
    /// High-priority work (e.g., direct user request, health checks).
    High = 3,
    /// Normal operational work (e.g., scheduled tasks).
    Normal = 2,
    /// Low-priority background work (e.g., periodic sync).
    Low = 1,
    /// Best-effort background work (e.g., pre-warming, analytics).
    Background = 0,
}

/// Returned when a request is shed due to backpressure.
#[derive(Debug, Clone)]
pub struct LoadShed {
    /// Current queue depth at the time of shedding.
    pub queue_depth: u64,
    /// The priority of the rejected request.
    pub priority: Priority,
}

impl std::fmt::Display for LoadShed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "load shed — queue depth {} exceeds threshold for {:?} priority",
            self.queue_depth, self.priority
        )
    }
}

impl std::error::Error for LoadShed {}

/// Configuration for the [`BackpressureController`].
#[derive(Debug, Clone)]
pub struct BackpressureConfig {
    /// Maximum queue depth before all non-critical work is shed.
    pub max_queue_depth: u64,
    /// Fraction of max_queue_depth at which Background work starts being shed (0.0–1.0).
    pub background_shed_threshold: f64,
    /// Fraction at which Low priority work starts being shed.
    pub low_shed_threshold: f64,
    /// Fraction at which Normal priority work starts being shed.
    pub normal_shed_threshold: f64,
    /// Fraction at which High priority work starts being shed.
    pub high_shed_threshold: f64,
}

impl Default for BackpressureConfig {
    fn default() -> Self {
        Self {
            max_queue_depth: 1000,
            background_shed_threshold: 0.5,
            low_shed_threshold: 0.7,
            normal_shed_threshold: 0.85,
            high_shed_threshold: 0.95,
        }
    }
}

/// Tracks queue depth and applies adaptive load shedding based on priority.
///
/// Call [`admit`](BackpressureController::admit) before enqueuing work, and
/// [`complete`](BackpressureController::complete) when the work finishes.
#[derive(Debug)]
pub struct BackpressureController {
    config: BackpressureConfig,
    current_depth: AtomicU64,
}

impl BackpressureController {
    /// Create a new controller with the given configuration.
    pub fn new(config: BackpressureConfig) -> Self {
        Self {
            config,
            current_depth: AtomicU64::new(0),
        }
    }

    /// Try to admit a request at the given priority.
    ///
    /// Returns `Ok(())` if the request is admitted (and increments the queue depth),
    /// or `Err(LoadShed)` if the request should be dropped.
    pub fn admit(&self, priority: Priority) -> Result<(), LoadShed> {
        let depth = self.current_depth.load(Ordering::Relaxed);

        // Critical requests are never shed.
        if priority == Priority::Critical {
            self.current_depth.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }

        // Guard: zero max_queue_depth means shed all non-critical requests immediately.
        if self.config.max_queue_depth == 0 {
            return Err(LoadShed {
                queue_depth: depth,
                priority,
            });
        }

        // Hard cap: reject everything non-critical at max.
        if depth >= self.config.max_queue_depth {
            return Err(LoadShed {
                queue_depth: depth,
                priority,
            });
        }

        let load_fraction = depth as f64 / self.config.max_queue_depth as f64;
        let threshold = match priority {
            Priority::Background => self.config.background_shed_threshold,
            Priority::Low => self.config.low_shed_threshold,
            Priority::Normal => self.config.normal_shed_threshold,
            Priority::High => self.config.high_shed_threshold,
            Priority::Critical => unreachable!(),
        };

        if load_fraction >= threshold {
            return Err(LoadShed {
                queue_depth: depth,
                priority,
            });
        }

        self.current_depth.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Signal that a previously admitted work item has completed.
    pub fn complete(&self) {
        // Saturating sub via CAS to avoid underflow.
        let _ = self
            .current_depth
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
                Some(v.saturating_sub(1))
            });
    }

    /// Returns the current queue depth.
    pub fn depth(&self) -> u64 {
        self.current_depth.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_controller(max: u64) -> BackpressureController {
        BackpressureController::new(BackpressureConfig {
            max_queue_depth: max,
            background_shed_threshold: 0.5,
            low_shed_threshold: 0.7,
            normal_shed_threshold: 0.85,
            high_shed_threshold: 0.95,
        })
    }

    #[test]
    fn admits_when_below_threshold() {
        let ctrl = make_controller(100);
        assert!(ctrl.admit(Priority::Normal).is_ok());
        assert_eq!(ctrl.depth(), 1);
    }

    #[test]
    fn complete_decrements_depth() {
        let ctrl = make_controller(100);
        ctrl.admit(Priority::Normal).unwrap();
        ctrl.admit(Priority::Normal).unwrap();
        assert_eq!(ctrl.depth(), 2);
        ctrl.complete();
        assert_eq!(ctrl.depth(), 1);
    }

    #[test]
    fn complete_does_not_underflow() {
        let ctrl = make_controller(100);
        ctrl.complete();
        assert_eq!(ctrl.depth(), 0);
    }

    #[test]
    fn sheds_background_first() {
        let ctrl = make_controller(10);

        // Fill to 50% (5 items) — should hit background threshold.
        for _ in 0..5 {
            ctrl.admit(Priority::High).unwrap();
        }

        // Background should be shed at 50%+.
        assert!(ctrl.admit(Priority::Background).is_err());
        // Normal should still be admitted (threshold 85%).
        assert!(ctrl.admit(Priority::Normal).is_ok());
    }

    #[test]
    fn sheds_low_at_threshold() {
        let ctrl = make_controller(10);

        // Fill to 70% (7 items).
        for _ in 0..7 {
            ctrl.admit(Priority::Critical).unwrap();
        }

        assert!(ctrl.admit(Priority::Low).is_err());
        // Normal still ok at 70% (threshold 85%).
        assert!(ctrl.admit(Priority::Normal).is_ok());
    }

    #[test]
    fn critical_never_shed() {
        let ctrl = make_controller(10);

        // Fill past max.
        for _ in 0..10 {
            ctrl.admit(Priority::Critical).unwrap();
        }

        // Non-critical should be shed.
        assert!(ctrl.admit(Priority::High).is_err());
        // Critical still admitted.
        assert!(ctrl.admit(Priority::Critical).is_ok());
    }

    #[test]
    fn sheds_all_non_critical_at_max() {
        let ctrl = make_controller(10);

        for _ in 0..10 {
            ctrl.admit(Priority::Critical).unwrap();
        }

        assert!(ctrl.admit(Priority::Background).is_err());
        assert!(ctrl.admit(Priority::Low).is_err());
        assert!(ctrl.admit(Priority::Normal).is_err());
        assert!(ctrl.admit(Priority::High).is_err());
    }

    #[test]
    fn zero_max_queue_depth_sheds_non_critical() {
        let ctrl = make_controller(0);
        // All non-critical should be shed immediately (no division by zero).
        assert!(ctrl.admit(Priority::Background).is_err());
        assert!(ctrl.admit(Priority::Low).is_err());
        assert!(ctrl.admit(Priority::Normal).is_err());
        assert!(ctrl.admit(Priority::High).is_err());
        // Critical still admitted.
        assert!(ctrl.admit(Priority::Critical).is_ok());
    }

    /// Integration-style test: simulate a wired admit/complete cycle to verify
    /// the controller tracks in-flight work correctly and sheds load at threshold.
    #[test]
    fn admit_complete_cycle_tracks_depth_and_sheds() {
        let ctrl = make_controller(10);

        // Admit 8 Normal requests (below 85% threshold).
        for _ in 0..8 {
            assert!(ctrl.admit(Priority::Normal).is_ok());
        }
        assert_eq!(ctrl.depth(), 8);

        // At depth 8/10 = 80%, Normal (threshold 85%) should still be admitted.
        assert!(ctrl.admit(Priority::Normal).is_ok());
        assert_eq!(ctrl.depth(), 9);

        // At depth 9/10 = 90%, Normal (threshold 85%) should be shed.
        assert!(ctrl.admit(Priority::Normal).is_err());
        assert_eq!(ctrl.depth(), 9); // depth unchanged on shed

        // Complete 3 items, bringing depth to 6.
        ctrl.complete();
        ctrl.complete();
        ctrl.complete();
        assert_eq!(ctrl.depth(), 6);

        // At depth 6/10 = 60%, Normal should be admitted again.
        assert!(ctrl.admit(Priority::Normal).is_ok());
        assert_eq!(ctrl.depth(), 7);
    }
}
