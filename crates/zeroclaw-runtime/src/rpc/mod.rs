//! Transport-agnostic JSON-RPC 2.0 dispatch for the runtime. See #6837.

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
