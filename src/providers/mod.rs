//! Provider subsystem — re-exported from `quantclaw-providers`.

pub use quantclaw_providers::*;

// Keep traits.rs as a file module so its #[cfg(test)] block compiles.
#[path = "traits.rs"]
pub mod traits;
