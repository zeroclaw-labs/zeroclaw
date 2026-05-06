//! ModelProvider subsystem — re-exported from `zeroclaw-model_providers`.

pub use zeroclaw_providers::*;

// Keep traits.rs as a file module so its #[cfg(test)] block compiles.
#[path = "traits.rs"]
pub mod traits;
