//! Transport-agnostic JSON-RPC 2.0 dispatch for the runtime. See #6837.

pub mod approval_channel;
pub mod attachments;
pub mod context;
pub mod dispatch;
pub mod fs;
pub mod git;
pub mod session;
pub mod transport;
pub mod tui_identity;
pub mod turn;
pub mod types;
#[cfg(unix)]
pub mod unix;
pub mod wss;
