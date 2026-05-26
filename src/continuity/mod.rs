pub mod commitments;
pub mod extraction;
pub mod guard;
pub mod identity;
pub mod narrative;
pub mod persistence;
pub mod preferences;
pub mod types;

pub use commitments::{check_fulfillment, extract_commitments};
pub use extraction::{extract_channel_preference, extract_tool_preference};
pub use guard::{prune_low_confidence, ContinuityGuard};
pub use identity::{
    compute_identity_checksum, identity_from_soul, identity_from_soul_with_epoch,
    verify_identity_rebuild,
};
pub use narrative::NarrativeStore;
pub use persistence::{
    continuity_dir, load_evolution_log, load_ledger, load_narrative, load_preferences,
    save_evolution_log, save_ledger, save_narrative, save_preferences, save_pruning_archive,
};
pub use preferences::PreferenceModel;
pub use types::{
    Commitment, ContinuitySnapshot, DriftLimits, Episode, Identity, IdentityCore, Preference,
    PreferenceCategory, PreferenceDelta, PreferenceSnapshot,
};

#[cfg(test)]
mod tests;
