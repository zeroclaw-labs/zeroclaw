//! Git-forge channel — converse with the agent through a forge's issue and
//! pull-request comments, and surface repository events (PR lifecycle,
//! review comments, CI outcomes, releases) through a per-event routing
//! table.
//!
//! Polling-based: inbound activity comes from the forge's REST API on
//! `since` cursors, plus an optional conditional-request feed backbone (no
//! webhook, no inbound network exposure). Outbound replies are comments
//! posted as the bot's identity, with draft streaming via comment edits.
//!
//! Architecture (contract-first, forge-agnostic core):
//! - [`types`] — generic constants, identifier newtypes, error enum.
//! - [`traits`] — the [`traits::GitProvider`] seam (+ fetch/identity/
//!   reaction value types).
//! - [`events`] — the generic [`events::GitEvent`] model, message mapping,
//!   and mention helpers.
//! - [`router`] — per-event routing policy over the config table.
//! - [`poll`] — pure cursor/dedup/ETag state machine.
//! - [`channel`] — the [`channel::GitChannel`] composition root over a
//!   boxed provider.
//! - [`providers`] — one impl per forge (GitHub first); adding a forge
//!   touches only this subtree.

mod channel;
mod events;
mod poll;
mod providers;
mod router;
mod traits;
mod types;

pub use channel::GitChannel;
