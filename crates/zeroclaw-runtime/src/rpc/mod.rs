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
