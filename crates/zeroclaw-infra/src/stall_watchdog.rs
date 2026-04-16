//! Generic transport stall watchdog for WebSocket-based channels.
//!
//! [`StallWatchdog`] detects when a channel transport goes idle beyond a
//! configurable threshold.  Channels call [`StallWatchdog::touch`] on every
//! received event; the watchdog fires a caller-supplied callback when the
//! elapsed silence exceeds `timeout_secs`.
//!
//! The timestamp is stored in an [`AtomicU64`] so `touch()` is lock-free and
//! safe to call from any async context.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Returns the current Unix timestamp in seconds.
fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A reusable watchdog that detects stalled (idle) WebSocket transports.
///
/// Create one per channel, call [`touch`](Self::touch) on every received
/// message or event, and [`start`](Self::start) with a callback that triggers
/// reconnection.
pub struct StallWatchdog {
    /// Unix timestamp (seconds) of the last received event.
    last_event: Arc<AtomicU64>,
    /// Stall threshold in seconds.
    timeout_secs: u64,
    /// Handle to the background polling task (if running).
    task: Mutex<Option<JoinHandle<()>>>,
}

impl StallWatchdog {
    /// Create a new watchdog with the given stall threshold.
    ///
    /// The watchdog is **not** started — call [`start`](Self::start) to begin
    /// monitoring.
    pub fn new(timeout_secs: u64) -> Self {
        Self {
            last_event: Arc::new(AtomicU64::new(now_secs())),
            timeout_secs,
            task: Mutex::new(None),
        }
    }

    /// Record that an event was received **right now**.
    ///
    /// This is lock-free (atomic store) and can be called from any async
    /// context without contention.
    pub fn touch(&self) {
        self.last_event.store(now_secs(), Ordering::Relaxed);
    }

    /// Returns `true` if the time since the last event exceeds the configured
    /// timeout.
    pub fn is_stalled(&self) -> bool {
        let last = self.last_event.load(Ordering::Relaxed);
        now_secs().saturating_sub(last) > self.timeout_secs
    }

    /// Start the background polling task.
    ///
    /// The task wakes every `timeout_secs / 2` seconds and checks whether the
    /// transport has stalled.  When a stall is detected `on_stall` is invoked
    /// (typically to log a warning and break out of the listen loop).
    ///
    /// Calling `start` while a task is already running replaces the previous
    /// task (the old one is aborted).
    pub async fn start(&self, on_stall: impl Fn() + Send + 'static) {
        // Reset timestamp so the freshly-started watchdog doesn't immediately
        // fire.
        self.touch();

        let last_event = Arc::clone(&self.last_event);
        let timeout = self.timeout_secs;
        let poll_interval = std::time::Duration::from_secs((timeout / 2).max(1));

        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(poll_interval);
            // The first tick completes immediately — skip it so we wait a full
            // interval before the first check.
            interval.tick().await;

            loop {
                interval.tick().await;
                let last = last_event.load(Ordering::Relaxed);
                if now_secs().saturating_sub(last) > timeout {
                    on_stall();
                    break;
                }
            }
        });

        let mut task = self.task.lock().await;
        if let Some(old) = task.take() {
            old.abort();
        }
        *task = Some(handle);
    }

    /// Stop the background polling task (if running).
    pub async fn stop(&self) {
        let mut task = self.task.lock().await;
        if let Some(handle) = task.take() {
            handle.abort();
        }
    }
}

impl Drop for StallWatchdog {
    fn drop(&mut self) {
        // Best-effort cleanup — abort the task synchronously if it exists.
        if let Ok(mut guard) = self.task.try_lock()
            && let Some(handle) = guard.take()
        {
            handle.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[tokio::test]
    async fn touch_prevents_stall() {
        let wd = StallWatchdog::new(2);
        wd.touch();
        assert!(!wd.is_stalled());
    }

    #[tokio::test]
    async fn is_stalled_after_timeout() {
        let wd = StallWatchdog::new(0); // 0-second timeout → always stalled
        // Force last_event into the past
        wd.last_event.store(0, Ordering::Relaxed);
        assert!(wd.is_stalled());
    }

    #[tokio::test]
    async fn callback_fires_on_stall() {
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let wd = StallWatchdog::new(1);

        wd.start(move || {
            fired_clone.store(true, Ordering::Relaxed);
        })
        .await;

        // Force last_event far into the past *after* start() so the next poll
        // detects a stall (start() calls touch() which would overwrite an
        // earlier store).
        wd.last_event.store(0, Ordering::Relaxed);

        // Wait long enough for the poll interval (1 / 2 = clamped to 1s) + margin.
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(fired.load(Ordering::Relaxed));
    }

    #[tokio::test]
    async fn stop_prevents_callback() {
        let fired = Arc::new(AtomicBool::new(false));
        let fired_clone = Arc::clone(&fired);

        let wd = StallWatchdog::new(1);

        wd.start(move || {
            fired_clone.store(true, Ordering::Relaxed);
        })
        .await;

        wd.last_event.store(0, Ordering::Relaxed);

        // Stop immediately before the poll task can fire.
        wd.stop().await;

        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        assert!(!fired.load(Ordering::Relaxed));
    }
}
