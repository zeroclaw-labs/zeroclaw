//! Native capability handlers registered with the local Tauri application.
//! Gateway-served content has no remote Tauri capability, so exposing these
//! commands requires a separately reviewed ACL design.

pub mod applescript;
pub mod screenshot;
