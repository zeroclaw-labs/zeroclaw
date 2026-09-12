//! Transport-agnostic JSON-RPC 2.0 dispatch for the runtime.

pub mod approval_channel;
pub mod attachments;
pub mod context;
pub mod dispatch;
pub mod fs;
pub mod git;
pub mod local;
pub mod locales;
pub mod session;
pub mod transport;
pub mod tui_identity;
pub mod turn;
pub mod types;
pub mod wss;

/// How long a cancelled listener waits for the connections it accepted to
/// finish draining before it force-aborts them. Sized above the turn
/// cancellation grace (`turn::CANCEL_GRACE`, 5 seconds) so a turn that unwinds
/// cooperatively is joined rather than aborted. The daemon reload path waits on
/// the same budget before it retires the listeners, so a replacement generation
/// cannot admit work while an old connection is still unwinding.
pub(crate) const CONNECTION_DRAIN_GRACE: std::time::Duration =
    std::time::Duration::from_millis(5500);

/// Liveness token for one accepted RPC connection, shared by every task that
/// connection started.
///
/// The listener's client counter is decremented when the LAST clone is
/// dropped, not when the connection task exits. A forced teardown aborts the
/// connection task and the prompt tasks it owns, but an abort only schedules
/// the drop of a task that may still be inside provider or tool cleanup. Each
/// of those tasks holds its own clone from inside its spawned body, so the
/// clone survives exactly as long as the task's future does, including the
/// nested turn task that runs the agent loop.
///
/// The daemon reads the same counter to decide when a replacement generation
/// may be admitted, so "the count reached zero" means "no task from the
/// retiring generation is still unwinding" rather than "the connection task
/// has been aborted".
pub struct ConnectionActivity(std::sync::Arc<ActivityCount>);

impl Clone for ConnectionActivity {
    fn clone(&self) -> Self {
        Self(std::sync::Arc::clone(&self.0))
    }
}

struct ActivityCount(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl ConnectionActivity {
    /// Count one live connection in `count` until every clone of the returned
    /// token has been dropped.
    pub(crate) fn new(count: std::sync::Arc<std::sync::atomic::AtomicUsize>) -> Self {
        count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Self(std::sync::Arc::new(ActivityCount(count)))
    }
}

impl std::fmt::Debug for ConnectionActivity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ConnectionActivity")
    }
}

impl Drop for ActivityCount {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}
