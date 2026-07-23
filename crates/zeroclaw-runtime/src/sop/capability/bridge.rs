//! Sync→async bridge for injected-adapter capabilities.
//!
//! [`super::SopCapability::execute`] is synchronous and runs while the caller
//! blocks a host thread (typically under the engine mutex, sometimes on a
//! current-thread runtime such as the channel dispatch context). Spawning the
//! async work back onto the HOST runtime is therefore unsound: on a
//! current-thread context the spawned task cannot be polled until the blocked
//! caller returns, which is a guaranteed timeout (observed in the field: the
//! model call only started executing the instant the capability gave up
//! waiting). Instead, each bridged call runs on a DEDICATED OS thread with its
//! own small current-thread runtime, fully independent of the host executor.
//! These calls are rare (one per side-effecting SOP step), so the per-call
//! thread + runtime cost is noise.

use std::future::Future;
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread::JoinHandle;
use std::time::Duration;

/// A spawned bridge worker: the result channel plus the worker's join handle.
type BridgeWorker<T> = (Receiver<Result<T, String>>, JoinHandle<()>);

fn spawn_bridge_thread<T, F>(fut: F, what: &str) -> Result<BridgeWorker<T>, String>
where
    T: Send + 'static,
    F: Future<Output = Result<T, String>> + Send + 'static,
{
    let (tx, rx) = sync_channel(1);
    let handle = std::thread::Builder::new()
        .name(format!("sop-bridge-{what}"))
        .spawn(move || {
            let result = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(rt) => rt.block_on(fut),
                Err(e) => Err(format!("bridge runtime build failed: {e}")),
            };
            // Receiver gone = caller timed out; nothing left to report to.
            let _ = tx.send(result);
        })
        .map_err(|e| format!("failed to spawn the {what} bridge thread: {e}"))?;
    Ok((rx, handle))
}

/// Run `fut` on a dedicated bridge thread, cancelling it after `timeout`.
/// `what` names the operation for thread naming and error text. The timeout runs
/// inside the bridge runtime, so its elapsed branch drops the pending future
/// before this function returns.
pub(super) fn run_bridged<T, F>(fut: F, timeout: Duration, what: &str) -> Result<T, String>
where
    T: Send + 'static,
    F: Future<Output = Result<T, String>> + Send + 'static,
{
    let timeout_error = format!(
        "timed out after {}s waiting for the {what}",
        timeout.as_secs()
    );
    let (rx, handle) = spawn_bridge_thread(
        async move {
            match tokio::time::timeout(timeout, fut).await {
                Ok(result) => result,
                Err(_) => Err(timeout_error),
            }
        },
        what,
    )?;
    match rx.recv() {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        // Sender dropped without a result: the bridged task died (panic) — a
        // different failure than a slow operation.
        Err(_) => {
            let _ = handle.join();
            Err(format!("{what} task died before reporting a result"))
        }
    }
}

/// Run `fut` on a dedicated bridge thread and return only after the worker has
/// completed, even if it exceeds `timeout`. This is for public, non-idempotent
/// writes: after the timeout expires, the caller waits for the eventual result
/// instead of taking a failure path while the write continues in the background.
pub(super) fn run_bridged_to_completion<T, F>(
    fut: F,
    timeout: Duration,
    what: &str,
) -> Result<T, String>
where
    T: Send + 'static,
    F: Future<Output = Result<T, String>> + Send + 'static,
{
    let (rx, handle) = spawn_bridge_thread(fut, what)?;
    match rx.recv_timeout(timeout) {
        Ok(result) => {
            let _ = handle.join();
            result
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => match handle.join() {
            Ok(()) => rx.recv().map_err(|_| {
                format!("{what} task died before reporting a result after timing out")
            })?,
            Err(_) => Err(format!("{what} task panicked after timing out")),
        },
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            let _ = handle.join();
            Err(format!("{what} task died before reporting a result"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    #[test]
    fn runs_a_future_from_a_plain_thread() {
        let out = run_bridged(
            async { Ok::<_, String>(7u32) },
            Duration::from_secs(5),
            "test",
        );
        assert_eq!(out, Ok(7));
    }

    #[test]
    fn runs_even_while_the_caller_blocks_inside_a_current_thread_runtime() {
        // The regression this bridge exists for: the caller blocks the ONLY
        // thread of a current-thread runtime while the bridged future must
        // still make progress (it cannot, if spawned onto that same runtime).
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let out = rt.block_on(async {
            run_bridged(
                async {
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    Ok::<_, String>("done".to_string())
                },
                Duration::from_secs(5),
                "test",
            )
        });
        assert_eq!(out, Ok("done".to_string()));
    }

    #[test]
    fn timeout_and_error_paths_are_distinguished() {
        let slow = run_bridged(
            async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<_, String>(0u8)
            },
            Duration::from_millis(50),
            "slowop",
        );
        assert!(slow.unwrap_err().contains("timed out"));

        let failing = run_bridged(
            async { Err::<u8, _>("boom".to_string()) },
            Duration::from_secs(5),
            "failop",
        );
        assert_eq!(failing.unwrap_err(), "boom");
    }

    struct DropMarker(Arc<AtomicBool>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[test]
    fn timeout_drops_the_pending_future_before_returning() {
        let dropped = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&dropped);
        let out = run_bridged(
            async move {
                let _marker = DropMarker(marker);
                std::future::pending::<()>().await;
                Ok::<_, String>(())
            },
            Duration::from_millis(50),
            "cancellable",
        );

        assert!(out.unwrap_err().contains("timed out"));
        assert!(
            dropped.load(Ordering::SeqCst),
            "the timed-out future must be dropped before the caller can retry"
        );
    }

    #[test]
    fn to_completion_waits_for_late_result_before_returning() {
        let side_effect = Arc::new(AtomicBool::new(false));
        let marker = Arc::clone(&side_effect);
        let started = Instant::now();

        let out = run_bridged_to_completion(
            async move {
                tokio::time::sleep(Duration::from_millis(75)).await;
                marker.store(true, Ordering::SeqCst);
                Ok::<_, String>("posted")
            },
            Duration::from_millis(10),
            "late write",
        );

        assert_eq!(out, Ok("posted"));
        assert!(started.elapsed() >= Duration::from_millis(50));
        assert!(
            side_effect.load(Ordering::SeqCst),
            "the late side effect must have completed before the caller proceeds"
        );
    }
}
