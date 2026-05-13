//! Channel infrastructure: session backends, debouncing, stall watchdog, and
//! shared channel statistics.
//!
//! These are cross-cutting utilities used by multiple channel implementations.

pub mod channel_stats;
pub mod debounce;
pub mod session_backend;
pub mod session_sqlite;
pub mod session_store;
pub mod stall_watchdog;
