use crate::hooks::HookHandler;
use crate::tools::traits::Tool;

/// Trait representing a unified ZeroClaw skill.
///
/// A skill combines tools, hooks, and system prompt contributions into a
/// single, pluggable unit of behavior.
pub trait Skill: Send + Sync {
    /// Unique name of the skill.
    fn name(&self) -> &str;

    /// Human-readable description of what the skill adds to the agent.
    fn description(&self) -> &str;

    /// Version of the skill (e.g. "1.0.0").
    fn version(&self) -> &str {
        "0.1.0"
    }

    /// Author of the skill.
    fn author(&self) -> Option<&str> {
        None
    }

    /// Tags associated with the skill.
    fn tags(&self) -> &[String] {
        &[]
    }

    /// Optional location of the skill on disk (for documentation/lookup).
    fn location(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Tools provided by this skill.
    fn tools(&self) -> Vec<Box<dyn Tool>> {
        vec![]
    }

    /// Lifecycle and event hooks provided by this skill.
    fn hooks(&self) -> Vec<Box<dyn HookHandler>> {
        vec![]
    }

    /// Optional text to be appended to the agent's system prompt.
    fn prompt_contribution(&self) -> Option<String> {
        None
    }

    /// Individual prompt instructions provided by this skill.
    fn prompts(&self) -> &[String] {
        &[]
    }
}
