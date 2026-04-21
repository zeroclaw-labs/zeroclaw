//! Onboard orchestrator — drives `config.set_prop` via the `OnboardUi` trait
//! (defined in `zeroclaw-config::traits`). Section flows, per-channel dispatch,
//! and the UI backends live under this module.

pub mod ui;
