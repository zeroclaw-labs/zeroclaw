//! Auto-creation trigger logic for procedural skills.
//!
//! After each agent turn, `maybe_create_skill` evaluates whether the turn
//! produced knowledge worth preserving as a reusable skill document.

use super::store::SkillStore;
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Summary of a completed agent turn — fed to the skill-worthiness evaluator.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnSummary {
    /// Number of tool calls made during this turn.
    pub tool_calls: usize,
    /// Whether the agent recovered from an error during execution.
    pub had_error_then_recovered: bool,
    /// Whether the user corrected the agent's output.
    pub user_corrected_output: bool,
    /// Category of the task (coding, document, daily, etc.).
    pub category: Option<String>,
    /// Brief description of what the turn accomplished.
    pub task_description: String,
    /// Full conversation snippet (for LLM analysis).
    pub conversation_snippet: String,
}

impl TurnSummary {
    /// Check if this turn matches a pattern already seen in existing skills.
    ///
    /// Uses OR-semantics FTS5 match so semantically similar tasks (overlapping
    /// keywords) are detected even when the phrasing differs.
    pub fn matches_existing_pattern(&self, store: &SkillStore) -> bool {
        let query = or_match_query(&self.task_description);
        if query.is_empty() {
            return false;
        }
        if let Ok(results) = store.search(&query, 3) {
            !results.is_empty()
        } else {
            false
        }
    }
}

/// Transform a free-text task description into an FTS5 OR-match query so that
/// partial keyword overlap counts as a "similar pattern" hit.
fn or_match_query(text: &str) -> String {
    text.split_whitespace()
        // Drop very short tokens and FTS5 metacharacters that would produce
        // parse errors when forwarded verbatim into the MATCH clause.
        .filter(|w| w.len() >= 2)
        .map(|w| w.trim_matches(|c: char| !c.is_alphanumeric()))
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// LLM verdict on whether a turn is worth saving as a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillWorthinessVerdict {
    pub worth_saving: bool,
    pub skill_name: String,
    pub description: String,
    pub category: Option<String>,
}

/// Result of the auto-creation attempt.
#[derive(Debug, Clone)]
pub struct AutoCreateResult {
    pub skill_id: String,
    pub skill_name: String,
}

/// System prompt for skill-worthiness judgement.
pub const SKILL_JUDGE_SYSTEM_PROMPT: &str = r#"You are evaluating whether an agent's completed task should be saved as a reusable skill.

A skill is worth saving when:
1. The task required 3+ tool calls OR multi-step reasoning
2. The agent learned something non-obvious (error recovery, domain knowledge)
3. The user corrected the output (indicating a preference worth remembering)
4. The pattern is likely to recur

A skill is NOT worth saving when:
1. It was a simple factual lookup
2. It was a one-time unique task unlikely to repeat
3. The knowledge is already captured in an existing skill

Respond in JSON format:
{
  "worth_saving": true/false,
  "skill_name": "kebab-case-name",
  "description": "One-line description",
  "category": "coding|document|daily|shopping|interpret|phone|image|music|video|null"
}"#;

/// System prompt for generating a SKILL.md document.
pub const SKILL_GEN_SYSTEM_PROMPT: &str = r#"Generate a SKILL.md document from the completed task.

The document must follow this structure:

# {Skill Name}

{One-paragraph description of what this skill covers and when to use it.}

## Procedure

{Step-by-step instructions for accomplishing this type of task.
Include specific commands, patterns, or approaches that worked.}

## Pitfalls

{Known failure modes, common mistakes, and things to watch out for.
Each pitfall on its own bullet point.}

## Verification

{How to verify the task was completed correctly.
Include specific checks or validation steps.}

## Context

- Category: {category}
- Created from: {brief task description}
- Key insight: {the most important non-obvious learning}

Write in the user's language. Be concise but thorough. Focus on actionable knowledge."#;

/// Evaluate a completed turn and create a skill if warranted.
///
/// This is the main entry point called from the agent loop's post-turn hook.
/// Returns `Some(AutoCreateResult)` if a skill was created, `None` otherwise.
///
/// The actual LLM calls are left to the caller — this function returns the
/// prompts needed and the caller passes back the LLM results. This keeps
/// the module free of provider dependencies.
pub fn should_trigger_skill_creation(turn: &TurnSummary, store: &SkillStore) -> bool {
    let dominated = [
        turn.tool_calls >= 3,
        turn.had_error_then_recovered,
        turn.user_corrected_output,
        turn.matches_existing_pattern(store),
    ];
    // At least one trigger condition must be true, but pattern match
    // is negative (we DON'T want to create duplicates).
    let positive_signals = dominated[..3].iter().filter(|&&b| b).count();
    let has_existing_pattern = dominated[3];

    positive_signals > 0 && !has_existing_pattern
}

/// Create a skill from an LLM-generated document.
///
/// Called after the LLM has judged the turn worthy and generated the SKILL.md.
pub fn maybe_create_skill(
    store: &SkillStore,
    verdict: &SkillWorthinessVerdict,
    generated_content: &str,
) -> Result<Option<AutoCreateResult>> {
    if !verdict.worth_saving {
        return Ok(None);
    }

    let skill_id = store.create(
        &verdict.skill_name,
        verdict.category.as_deref(),
        &verdict.description,
        generated_content,
        "agent",
    )?;

    Ok(Some(AutoCreateResult {
        skill_id,
        skill_name: verdict.skill_name.clone(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use rusqlite::Connection;
    use std::sync::Arc;

    fn test_store() -> SkillStore {
        let conn = Connection::open_in_memory().unwrap();
        super::super::schema::migrate(&conn).unwrap();
        SkillStore::new(Arc::new(Mutex::new(conn)), "test-device".into())
    }

    #[test]
    fn trigger_on_complex_task() {
        let store = test_store();
        let turn = TurnSummary {
            tool_calls: 5,
            had_error_then_recovered: true,
            user_corrected_output: false,
            category: Some("coding".into()),
            task_description: "Fixed borrow checker issue in sync module".into(),
            conversation_snippet: String::new(),
        };
        assert!(should_trigger_skill_creation(&turn, &store));
    }

    #[test]
    fn no_trigger_on_simple_task() {
        let store = test_store();
        let turn = TurnSummary {
            tool_calls: 1,
            had_error_then_recovered: false,
            user_corrected_output: false,
            category: Some("daily".into()),
            task_description: "What's the weather?".into(),
            conversation_snippet: String::new(),
        };
        assert!(!should_trigger_skill_creation(&turn, &store));
    }

    #[test]
    fn no_trigger_when_pattern_exists() {
        let store = test_store();
        store
            .create("borrow-fix", Some("coding"), "Fix borrow checker", "# Fix\n\n...", "agent")
            .unwrap();
        let turn = TurnSummary {
            tool_calls: 5,
            had_error_then_recovered: true,
            user_corrected_output: false,
            category: Some("coding".into()),
            task_description: "Fix borrow checker issue".into(),
            conversation_snippet: String::new(),
        };
        // Pattern exists — should not trigger duplicate creation
        assert!(!should_trigger_skill_creation(&turn, &store));
    }

    #[test]
    fn create_skill_from_verdict() {
        let store = test_store();
        let verdict = SkillWorthinessVerdict {
            worth_saving: true,
            skill_name: "hwp-table-fix".into(),
            description: "HWP table rendering fix pattern".into(),
            category: Some("document".into()),
        };
        let result = maybe_create_skill(&store, &verdict, "# HWP Table Fix\n\n...").unwrap();
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.skill_name, "hwp-table-fix");

        let skill = store.get(&r.skill_id).unwrap().unwrap();
        assert_eq!(skill.created_by, "agent");
    }
}
