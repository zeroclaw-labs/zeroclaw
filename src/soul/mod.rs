//! Soul system — optional identity, constitution, and alignment tracking.
//!
//! Ported from Conway-Research/automaton concepts into ZeroClaw's
//! trait-driven architecture. The soul system provides:
//!
//! - **SoulModel**: Structured identity (name, values, personality, boundaries)
//! - **Constitution**: Immutable laws with SHA-256 integrity verification
//! - **Alignment**: Genesis drift tracking (Jaccard similarity + recall)
//! - **Parser**: SOUL.md (YAML frontmatter + markdown sections) loader
//! - **Survival**: Tier-based resource management (credit balance → behavior)
//! - **ModelStrategy**: Tier-based model selection with budget enforcement
//! - **Replication**: Child agent spawn lifecycle with constitution propagation

pub mod alignment;
pub mod constitution;
pub mod model;
// References removed ModelStrategyConfig; gated behind x0-broken-legacy
// until the soul model-routing strategy is rebuilt against V3 providers.
#[cfg(feature = "x0-broken-legacy")]
pub mod model_strategy;
pub mod parser;
pub mod reflection;
// References removed ReplicationConfig; gated behind x0-broken-legacy
// until the soul replication path is rebuilt against V3.
#[cfg(feature = "x0-broken-legacy")]
pub mod replication;
pub mod survival;

pub use alignment::AlignmentScore;
pub use constitution::Constitution;
#[cfg(feature = "x0-broken-legacy")]
#[allow(unused_imports)]
pub use model_strategy::{ModelStrategy, TierModelOverride};
pub use parser::parse_soul_file;
#[allow(unused_imports)]
pub use reflection::{MemoryTokenBudgets, ReflectionInsights};
#[cfg(feature = "x0-broken-legacy")]
#[allow(unused_imports)]
pub use replication::{ReplicationError, ReplicationManager, ReplicationPhase};
#[allow(unused_imports)]
pub use survival::{SurvivalMonitor, SurvivalStatus, SurvivalThresholds, SurvivalTier};
