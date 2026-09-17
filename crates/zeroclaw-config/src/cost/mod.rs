pub mod tracker;
pub mod types;
pub use tracker::CostTracker;
pub use types::*;

/// Upper bound for any configured or discovered USD cost rate.
///
/// For model tokens this is $1 per token, orders of magnitude above current
/// provider prices. Keeping the bound in the config cost module gives config,
/// runtime recording, and live-provider normalization one policy source.
pub const MAX_SANE_USD_RATE: f64 = 1_000_000.0;

/// Whether a USD rate is finite, non-negative, and within the shared safety
/// bound. Zero remains valid so an explicitly free resource is distinguishable
/// from unavailable pricing.
#[must_use]
pub fn is_sane_usd_rate(rate: f64) -> bool {
    (0.0..=MAX_SANE_USD_RATE).contains(&rate)
}
