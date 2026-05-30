use serde::{Deserialize, Serialize};

/// How much autonomy the agent has.
///
/// Variants are ordered from least to most autonomous so that
/// [`Ord`] / [`PartialOrd`] compare a child's level against a
/// parent's during SubAgent escalation checks (`child <= parent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(rename_all = "lowercase")]
pub enum AutonomyLevel {
    /// Read-only: can observe but not act
    ReadOnly,
    /// Supervised: acts but requires approval for risky operations
    #[default]
    Supervised,
    /// Full: autonomous execution within policy bounds
    Full,
}
