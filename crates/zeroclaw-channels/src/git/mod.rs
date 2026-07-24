//! Git-forge channel — converse with the agent through a forge's issue and
//! pull-request comments, and surface repository events (PR lifecycle,
//! review comments, CI outcomes, releases) through a per-event routing
//! table.

mod channel;
mod events;
mod poll;
mod providers;
mod router;
mod traits;
mod types;

pub use channel::GitChannel;
