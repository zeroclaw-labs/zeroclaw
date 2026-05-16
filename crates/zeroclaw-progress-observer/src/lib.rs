//! Sidelined progress observer for ZeroClaw.
//!
//! This crate hosts a single [`Observer`](zeroclaw_api::observability_traits::Observer)
//! implementation that translates selected [`ObserverEvent`]s into
//! [`StatusUpdate`]s and ships them to a target [`Channel`] via the
//! channel's [`send_status_update`](zeroclaw_api::channel::Channel::send_status_update)
//! method. The observer is per-message: orchestrator constructs a fresh
//! instance for each `ChannelMessage` and drops it when the turn ends.

mod toggles;
mod mapping;
mod observer;

#[cfg(test)]
mod mock;

pub use toggles::ProgressEventToggles;
// pub use observer::ProgressReportingObserver;
