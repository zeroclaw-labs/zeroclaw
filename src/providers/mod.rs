//! Provider subsystem — re-exported from `daemonclaw-providers`.

pub use daemonclaw_providers::*;

// Keep traits.rs as a file module so its #[cfg(test)] block compiles.
#[path = "traits.rs"]
pub mod traits;
