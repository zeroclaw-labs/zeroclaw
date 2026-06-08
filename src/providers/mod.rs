//! ModelProvider subsystem — re-exported from `zeroclaw-providers`.

pub use zeroclaw_providers::*;

// Legacy alias: the trait was renamed `Provider` -> `ModelProvider` during the
// crate split. Pre-split callers still `use crate::providers::Provider`.
pub use zeroclaw_api::model_provider::ModelProvider as Provider;

// Provider-alias helper now lives in `zeroclaw_config::provider_aliases`; the
// onboard wizard imports it from `crate::providers`.
pub use zeroclaw_config::provider_aliases::canonical_china_provider_name;

// Keep traits.rs as a file module so its #[cfg(test)] block compiles.
#[path = "traits.rs"]
pub mod traits;
