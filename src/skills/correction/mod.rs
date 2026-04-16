//! Self-Learning Correction Skill — learn from user edits.
//!
//! Observes user text edits, validates that corrections are grammatically sound,
//! mines repeated patterns, and recommends corrections in new documents —
//! all from the user's actual editing behavior rather than generic grammar rules.
//!
//! Pipeline: observe → validate → mine → recommend → feedback
//!
//! This is a concrete implementation of the broader "skill system +
//! user behavior modeling" framework, scoped to the document category
//! but directly extensible to coding, interpret, image, etc.

pub mod applier;
pub mod factory;
pub mod grammar_checker;
pub mod observer;
pub mod pattern_miner;
pub mod recommender;
pub mod schema;
pub mod store;

pub use applier::{apply_feedback, UserAction};
pub use factory::build_store;
pub use grammar_checker::{validate_correction, ValidationResult, ValidationVerdict};
pub use observer::{observe_edit, CorrectionObservation};
pub use pattern_miner::{mine_patterns, PatternUpdate};
pub use recommender::{scan_and_recommend, CorrectionRecommendation};
pub use store::{CorrectionPattern, CorrectionStore, PatternType};
