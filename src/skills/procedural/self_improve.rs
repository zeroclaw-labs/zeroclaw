//! Self-improvement logic for procedural skills.
//!
//! When a skill is used during task execution and errors occur or the user
//! corrects the output, this module patches the skill document to incorporate
//! the new knowledge.

use super::store::{PatchTarget, SkillStore};
use anyhow::Result;
use serde::{Deserialize, Serialize};

/// Result of a task execution that used a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether any errors occurred during execution.
    pub had_errors: bool,
    /// Whether the user edited the agent's output after completion.
    pub user_edited_output: bool,
    /// Whether the task ultimately succeeded.
    pub succeeded: bool,
    /// Error context (for pitfall extraction).
    pub error_context: Option<String>,
    /// User's edits (for procedure revision).
    pub user_edits: Option<String>,
    /// The skill ID that was used.
    pub skill_id: String,
}

/// System prompt for extracting lessons from execution errors.
pub const LESSON_EXTRACT_PROMPT: &str = r#"A skill was used to guide a task, but an error occurred.
Extract a concise pitfall warning (1-2 sentences) that should be added to the skill's "Pitfalls" section.
Focus on: what went wrong, why, and how to avoid it next time.
Write in the same language as the error context."#;

/// System prompt for revising a skill's procedure based on user corrections.
pub const PROCEDURE_REVISE_PROMPT: &str = r#"A skill's procedure was followed, but the user corrected the output.
Revise the procedure to incorporate the user's preferred approach.
Keep the same structure (step-by-step) but adjust the steps to produce
the output the user wanted. Be concise and actionable."#;

/// Apply post-execution improvements to a skill.
///
/// This is designed to be called with pre-computed LLM outputs. The caller
/// is responsible for making the LLM calls using the prompts above.
pub fn improve_after_execution(
    store: &SkillStore,
    result: &ExecutionResult,
    extracted_lesson: Option<&str>,
    revised_procedure: Option<&str>,
) -> Result<ImprovementReport> {
    let mut report = ImprovementReport::default();

    if result.had_errors {
        if let Some(lesson) = extracted_lesson {
            store.patch(&result.skill_id, PatchTarget::Pitfalls, lesson)?;
            report.pitfall_added = true;
        }
    }

    if result.user_edited_output {
        if let Some(revised) = revised_procedure {
            store.patch(&result.skill_id, PatchTarget::Procedure, revised)?;
            report.procedure_revised = true;
        }
    }

    // Always record usage stats
    store.record_usage(&result.skill_id, result.succeeded)?;
    report.usage_recorded = true;

    Ok(report)
}

/// Report of what improvements were applied.
#[derive(Debug, Default)]
pub struct ImprovementReport {
    /// Whether a new pitfall was appended.
    pub pitfall_added: bool,
    /// Whether the procedure was revised.
    pub procedure_revised: bool,
    /// Whether usage stats were recorded.
    pub usage_recorded: bool,
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
    fn improve_adds_pitfall_on_error() {
        let store = test_store();
        let id = store
            .create(
                "test-skill",
                None,
                "test",
                "# Test\n\n## Procedure\nStep 1\n\n## Pitfalls\n- Existing",
                "agent",
            )
            .unwrap();

        let result = ExecutionResult {
            had_errors: true,
            user_edited_output: false,
            succeeded: true,
            error_context: Some("timeout on large files".into()),
            user_edits: None,
            skill_id: id.clone(),
        };

        let report =
            improve_after_execution(&store, &result, Some("Large files cause timeout — add chunking"), None)
                .unwrap();
        assert!(report.pitfall_added);
        assert!(!report.procedure_revised);

        let skill = store.get(&id).unwrap().unwrap();
        assert!(skill.content_md.contains("Large files cause timeout"));
        assert_eq!(skill.version, 2);
        assert_eq!(skill.use_count, 1);
    }

    #[test]
    fn improve_revises_procedure_on_user_edit() {
        let store = test_store();
        let id = store
            .create(
                "test-skill",
                None,
                "test",
                "# Test\n\n## Procedure\nOld procedure\n\n## Pitfalls\n- None",
                "agent",
            )
            .unwrap();

        let result = ExecutionResult {
            had_errors: false,
            user_edited_output: true,
            succeeded: true,
            error_context: None,
            user_edits: Some("User preferred different approach".into()),
            skill_id: id.clone(),
        };

        let report = improve_after_execution(
            &store,
            &result,
            None,
            Some("1. New step A\n2. New step B"),
        )
        .unwrap();
        assert!(!report.pitfall_added);
        assert!(report.procedure_revised);

        let skill = store.get(&id).unwrap().unwrap();
        assert!(skill.content_md.contains("New step A"));
    }
}
