//! Single-flight serialization for per-resource token refreshes.
//!
//! Two concurrent MCP requests that both find an expired token must not
//! issue two refreshes: public-client refresh tokens rotate, and a
//! server-side replay guard can kill the grant (or the family) when the
//! same refresh token is redeemed twice. This type hands every refresh for
//! one canonical resource through one async mutex, so the caller's
//! check-refresh-save sequence becomes an atomic unit across concurrent
//! callers in this process.
//!
//! Scope: in-process only. The credential file's own lock covers
//! read-modify-write, but two ZeroClaw processes refreshing the same
//! resource simultaneously can still race; the second then fails its
//! exchange with `invalid_grant`, and the transport's
//! `ReauthorizationRequired` path (not replay) is the recovery.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

/// Per-key async mutexes, created on demand and kept for the process
/// lifetime. The key space is the set of configured canonical resources,
/// which is config-bounded and small; entries are never removed, which also
/// avoids the remove-while-held race where a waiter holds a mutex that a
/// completer already deleted (two subsequent callers could then pair with
/// different mutexes and run at once).
#[derive(Debug, Default, Clone)]
pub struct RefreshCoordinator {
    locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

impl RefreshCoordinator {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock_for(&self, key: &str) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = self.locks.lock();
        locks
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    }

    /// Run `op` exclusively for `key`: every other `run_exclusive` on the
    /// same key waits for this one to finish. NOT reentrant: calling
    /// `run_exclusive` for the same key from inside a live section
    /// self-deadlocks (design §12 makes the ban binding for callers).
    /// The caller re-checks state
    /// inside `op` (double-checked locking): a waiter whose predecessor
    /// just refreshed should observe the fresh grant and skip its own
    /// refresh, which is the whole point of the coordination.
    pub async fn run_exclusive<F, T>(&self, key: &str, op: F) -> T
    where
        F: Future<Output = T>,
    {
        let mutex = self.lock_for(key);
        let _guard = mutex.lock().await;
        op.await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    async fn sleep_ms(ms: u64) {
        tokio::time::sleep(Duration::from_millis(ms)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_key_runs_exclusively() {
        let coordinator = RefreshCoordinator::new();
        let concurrent = Arc::new(AtomicUsize::new(0));
        let max_seen = Arc::new(AtomicUsize::new(0));

        let mut tasks = Vec::new();
        for _ in 0..6 {
            let coordinator = coordinator.clone();
            let concurrent = concurrent.clone();
            let max_seen = max_seen.clone();
            tasks.push(zeroclaw_spawn::spawn!(async move {
                coordinator
                    .run_exclusive("resource-a", async {
                        let now = concurrent.fetch_add(1, Ordering::SeqCst) + 1;
                        max_seen.fetch_max(now, Ordering::SeqCst);
                        sleep_ms(20).await;
                        concurrent.fetch_sub(1, Ordering::SeqCst);
                    })
                    .await;
            }));
        }
        for task in tasks {
            task.await.expect("task");
        }
        assert_eq!(
            max_seen.load(Ordering::SeqCst),
            1,
            "two same-key refresh sections overlapped"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn different_keys_run_concurrently() {
        let coordinator = RefreshCoordinator::new();
        let barrier = Arc::new(tokio::sync::Barrier::new(2));

        let left = {
            let coordinator = coordinator.clone();
            let barrier = barrier.clone();
            zeroclaw_spawn::spawn!(async move {
                coordinator
                    .run_exclusive("resource-a", async {
                        // If keys were shared, this would time out the
                        // barrier instead of both tasks arriving.
                        barrier.wait().await;
                    })
                    .await;
            })
        };
        let right = {
            let barrier = barrier;
            zeroclaw_spawn::spawn!(async move {
                coordinator
                    .run_exclusive("resource-b", async {
                        barrier.wait().await;
                    })
                    .await;
            })
        };
        tokio::time::timeout(Duration::from_secs(5), left)
            .await
            .expect("distinct keys must not serialize")
            .expect("task");
        tokio::time::timeout(Duration::from_secs(5), right)
            .await
            .expect("distinct keys must not serialize")
            .expect("task");
    }

    #[tokio::test]
    async fn waiter_observes_predecessor_state() {
        // The documented usage pattern: re-check inside the section. The
        // second caller must see whatever the first committed.
        let coordinator = RefreshCoordinator::new();
        let committed = Arc::new(AtomicUsize::new(0));

        let first = {
            let coordinator = coordinator.clone();
            let committed = committed.clone();
            zeroclaw_spawn::spawn!(async move {
                coordinator
                    .run_exclusive("r", async {
                        sleep_ms(30).await;
                        committed.store(1, Ordering::SeqCst);
                    })
                    .await;
            })
        };
        // Ensure the first task genuinely holds the section before the
        // second arrives, making the overlap (not just ordering) explicit.
        sleep_ms(5).await;
        let second = {
            let coordinator = coordinator.clone();
            let committed = committed.clone();
            zeroclaw_spawn::spawn!(async move {
                coordinator
                    .run_exclusive("r", async {
                        // Double-check: the refresh would be skipped here in
                        // the real flow when the grant is already fresh.
                        assert_eq!(committed.load(Ordering::SeqCst), 1);
                    })
                    .await;
            })
        };
        first.await.expect("first");
        second.await.expect("second");
    }
}
