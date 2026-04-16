//! Turn/Session lifecycle hooks for procedural skills.
//!
//! Integration points for the agent loop:
//! - `on_turn_complete`: evaluate whether to create a skill from this turn
//! - `on_skill_used`: apply improvements when a skill was used during the turn
//! - `build_prompt_injection`: L0 skill index for system prompt
//!
//! These hooks are intentionally simple and callable — the agent loop
//! just needs to invoke them at the right points without worrying about
//! FTS5, SQL, or sync plumbing.

use super::auto_create::{should_trigger_skill_creation, TurnSummary};
use super::progressive::{inject_skill_index, SkillSummary};
use super::store::SkillStore;
use anyhow::Result;
use std::sync::Arc;

/// Evaluate a completed turn for skill-worthiness.
///
/// Returns `true` if the turn meets the trigger heuristics (complex work,
/// error recovery, or user correction — with no pre-existing matching skill).
/// The caller should then prompt the LLM with
/// `auto_create::SKILL_JUDGE_SYSTEM_PROMPT` for a final worthiness verdict.
pub fn should_trigger(turn: &TurnSummary, store: &SkillStore) -> bool {
    should_trigger_skill_creation(turn, store)
}

/// Build the L0 skill index for injecting into the system prompt.
///
/// Lists all known skills with category, description, and usage stats.
/// The agent reads this and decides when to call `skill_view(name)` to
/// fetch a specific skill's full content.
pub fn build_prompt_injection(store: &SkillStore) -> Result<String> {
    let records = store.list_all()?;
    let summaries: Vec<SkillSummary> = records.iter().map(SkillSummary::from).collect();
    Ok(inject_skill_index(&summaries))
}

/// Convenience shim — same as `build_prompt_injection` but takes an Arc.
pub fn build_prompt_injection_arc(store: &Arc<SkillStore>) -> Result<String> {
    build_prompt_injection(store.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;

    fn test_store() -> Arc<SkillStore> {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        Arc::new(SkillStore::new(
            Arc::new(Mutex::new(conn)),
            "test-device".into(),
        ))
    }

    #[test]
    fn empty_store_produces_empty_injection() {
        let store = test_store();
        let inj = build_prompt_injection(&store).unwrap();
        assert!(inj.is_empty());
    }

    #[test]
    fn populated_store_includes_skills() {
        let store = test_store();
        store
            .create(
                "rust-borrow",
                Some("coding"),
                "Rust borrow checker patterns",
                "# Rust Borrow\n\n## Procedure\n...",
                "agent",
            )
            .unwrap();
        let inj = build_prompt_injection(&store).unwrap();
        assert!(inj.contains("rust-borrow"));
        assert!(inj.contains("coding"));
        assert!(inj.contains("skill_view"));
    }
}
