//! Procedural Memory — Self-generating skill system.
//!
//! Skills are auto-created by the agent from successful complex task completions,
//! then self-improved during subsequent use. This module implements:
//!
//! - `SkillStore`: CRUD + FTS5 search for skills in brain.db
//! - `auto_create`: Post-turn trigger logic for skill auto-generation
//! - `self_improve`: Patch skills when errors/corrections occur during use
//! - `progressive`: L0/L1/L2 progressive disclosure for token budget control
//! - `sync`: DeltaOperation::SkillUpsert for multi-device replication

pub mod auto_create;
pub mod factory;
pub mod lifecycle;
pub mod progressive;
pub mod schema;
pub mod self_improve;
pub mod store;
pub mod sync;

pub use auto_create::maybe_create_skill;
pub use factory::build_store;
pub use lifecycle::{build_prompt_injection, should_trigger};
pub use progressive::{inject_skill_index, SkillDepth, SkillSummary};
pub use self_improve::improve_after_execution;
pub use store::SkillStore;
pub use sync::SkillUpsertDelta;
