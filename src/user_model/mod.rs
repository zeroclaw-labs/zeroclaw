//! Cross-Session User Profiling — behavioral pattern modeling.
//!
//! Observes conversation patterns across sessions to build a persistent
//! user profile: response style preferences, expertise levels, work patterns,
//! decision styles, tool preferences, and feedback patterns.
//!
//! The profile is injected into the system prompt so the agent can
//! personalize responses without the user repeating preferences.

pub mod factory;
pub mod profiler;
pub mod schema;

pub use factory::build_profiler;
pub use profiler::{ProfileConclusion, UserProfiler};
