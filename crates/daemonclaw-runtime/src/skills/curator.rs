use std::sync::Arc;

use crate::hooks::builtin::background_llm::{BackgroundLlmConfig, background_llm_call};
use crate::observability::{Observer, ObserverEvent};
use crate::skills::store::SkillStore;
use crate::skills::types::AgentSkill;

const GRADE_PROMPT_TEMPLATE: &str = r#"You are a skill quality curator. Grade the following agent-created skill on a scale of 1-5.

Criteria:
- Clarity: Is the procedure clear and actionable?
- Specificity: Does it describe concrete steps, not vague guidance?
- Reusability: Would this be useful across multiple sessions?
- Completeness: Does it cover edge cases and error handling?

Respond with exactly one line: GRADE:<1-5> followed by a brief reason.

Example: GRADE:4 Clear deployment procedure but missing rollback steps.

## Skill: {{name}}

Description: {{description}}

{{body}}"#;

const CONSOLIDATION_PROMPT_TEMPLATE: &str = r#"You have two agent-created skills that may overlap. Decide whether to consolidate them.

Respond with exactly one of:
- KEEP_BOTH — they are distinct enough to remain separate
- MERGE_INTO_FIRST — merge the second into the first (respond with the merged body after this line)
- MERGE_INTO_SECOND — merge the first into the second (respond with the merged body after this line)

## Skill A: {{name_a}}
{{body_a}}

## Skill B: {{name_b}}
{{body_b}}"#;

/// Result of a single skill review.
#[derive(Debug)]
pub struct SkillReview {
    pub name: String,
    pub grade: u8,
    pub reason: String,
}

/// Summary of a full curator run.
#[derive(Debug, Default)]
pub struct CuratorRunSummary {
    pub skills_reviewed: usize,
    pub skills_archived: usize,
    pub skills_consolidated: usize,
}

/// Run the curator over all agent-created skills.
///
/// This function:
/// 1. Skips pinned skills and skills with active lockfiles
/// 2. Grades each skill via background LLM call
/// 3. Archives skills graded 1 (or unused for too long)
/// 4. Checks for overlapping skills and offers consolidation
/// 5. Emits CuratorRunCompleted observer event
pub async fn run_curator(
    store: &SkillStore,
    llm_config: &BackgroundLlmConfig,
    observer: &Arc<dyn Observer>,
    min_grade: u8,
) -> CuratorRunSummary {
    store.cleanup_stale_active_files();

    let agent_skills = store.list_agent();
    if agent_skills.is_empty() {
        return CuratorRunSummary::default();
    }

    let mut summary = CuratorRunSummary::default();
    let mut reviews: Vec<SkillReview> = Vec::new();

    for skill in &agent_skills {
        if skill.meta().pinned {
            tracing::debug!(target: "curator", name = %skill.name(), "skipping pinned skill");
            continue;
        }

        if SkillStore::is_active(&skill.dir_path) {
            tracing::debug!(target: "curator", name = %skill.name(), "skipping active skill");
            continue;
        }

        summary.skills_reviewed += 1;

        let review = grade_skill(skill, llm_config, observer).await;
        match &review {
            Some(r) if r.grade < min_grade => {
                if let Err(e) = store.archive(skill.name()) {
                    tracing::warn!(target: "curator", name = %skill.name(), "failed to archive: {e}");
                } else {
                    tracing::info!(target: "curator", name = %skill.name(), grade = r.grade, "archived low-quality skill");
                    observer.record_event(&ObserverEvent::SkillArchived {
                        skill_name: skill.name().to_string(),
                    });
                    summary.skills_archived += 1;
                }
            }
            Some(r) => {
                reviews.push(SkillReview {
                    name: skill.name().to_string(),
                    grade: r.grade,
                    reason: r.reason.clone(),
                });
            }
            None => {
                reviews.push(SkillReview {
                    name: skill.name().to_string(),
                    grade: 3,
                    reason: "grading failed, keeping".into(),
                });
            }
        }
    }

    // Consolidation pass: check pairs of remaining skills
    if reviews.len() >= 2 {
        let consolidated = check_consolidation(store, &reviews, llm_config, observer).await;
        summary.skills_consolidated = consolidated;
    }

    observer.record_event(&ObserverEvent::CuratorRunCompleted {
        skills_reviewed: summary.skills_reviewed,
        skills_archived: summary.skills_archived,
        skills_consolidated: summary.skills_consolidated,
    });

    tracing::info!(
        target: "curator",
        reviewed = summary.skills_reviewed,
        archived = summary.skills_archived,
        consolidated = summary.skills_consolidated,
        "curator run complete"
    );

    summary
}

async fn grade_skill(
    skill: &AgentSkill,
    llm_config: &BackgroundLlmConfig,
    observer: &Arc<dyn Observer>,
) -> Option<SkillReview> {
    let prompt = GRADE_PROMPT_TEMPLATE
        .replace("{{name}}", skill.name())
        .replace("{{description}}", skill.description())
        .replace("{{body}}", &skill.body);

    let response = background_llm_call(llm_config, &prompt, Some(observer)).await?;
    parse_grade_response(skill.name(), &response)
}

fn parse_grade_response(name: &str, response: &str) -> Option<SkillReview> {
    let trimmed = response.trim();

    for line in trimmed.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("GRADE:") {
            let rest = rest.trim();
            let grade_char = rest.chars().next()?;
            let grade: u8 = grade_char.to_digit(10)? as u8;
            if !(1..=5).contains(&grade) {
                continue;
            }
            let reason = rest[1..].trim().trim_start_matches(' ').to_string();
            return Some(SkillReview {
                name: name.to_string(),
                grade,
                reason,
            });
        }
    }

    None
}

async fn check_consolidation(
    store: &SkillStore,
    reviews: &[SkillReview],
    llm_config: &BackgroundLlmConfig,
    observer: &Arc<dyn Observer>,
) -> usize {
    let mut consolidated = 0;

    // Only check sequential pairs to limit LLM calls
    for i in 0..reviews.len().saturating_sub(1) {
        let a = &reviews[i];
        let b = &reviews[i + 1];

        let skill_a = match store.get_agent(&a.name) {
            Ok(Some(s)) => s,
            _ => continue,
        };
        let skill_b = match store.get_agent(&b.name) {
            Ok(Some(s)) => s,
            _ => continue,
        };

        let prompt = CONSOLIDATION_PROMPT_TEMPLATE
            .replace("{{name_a}}", &a.name)
            .replace("{{body_a}}", &skill_a.body)
            .replace("{{name_b}}", &b.name)
            .replace("{{body_b}}", &skill_b.body);

        let response = match background_llm_call(llm_config, &prompt, Some(observer)).await {
            Some(r) => r,
            None => continue,
        };

        let first_line = response.lines().next().unwrap_or("").trim().to_uppercase();

        if first_line.starts_with("MERGE_INTO_FIRST") {
            let merged_body = response
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if !merged_body.is_empty() {
                let mut fm = skill_a.frontmatter.clone();
                fm.metadata.updated = Some(chrono::Utc::now().to_rfc3339());
                if store.write_agent(&a.name, &fm, &merged_body).is_ok()
                    && store.archive(&b.name).is_ok()
                {
                    tracing::info!(target: "curator", "consolidated '{}' into '{}'", b.name, a.name);
                    observer.record_event(&ObserverEvent::SkillArchived {
                        skill_name: b.name.clone(),
                    });
                    consolidated += 1;
                }
            }
        } else if first_line.starts_with("MERGE_INTO_SECOND") {
            let merged_body = response
                .lines()
                .skip(1)
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string();
            if !merged_body.is_empty() {
                let mut fm = skill_b.frontmatter.clone();
                fm.metadata.updated = Some(chrono::Utc::now().to_rfc3339());
                if store.write_agent(&b.name, &fm, &merged_body).is_ok()
                    && store.archive(&a.name).is_ok()
                {
                    tracing::info!(target: "curator", "consolidated '{}' into '{}'", a.name, b.name);
                    observer.record_event(&ObserverEvent::SkillArchived {
                        skill_name: a.name.clone(),
                    });
                    consolidated += 1;
                }
            }
        }
    }

    consolidated
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_grade_valid() {
        let review = parse_grade_response(
            "deploy-nginx",
            "GRADE:4 Clear deployment procedure but missing rollback steps.",
        )
        .unwrap();
        assert_eq!(review.grade, 4);
        assert!(review.reason.contains("rollback"));
    }

    #[test]
    fn parse_grade_low() {
        let review = parse_grade_response(
            "bad-skill",
            "GRADE:1 Vague and not reusable.",
        )
        .unwrap();
        assert_eq!(review.grade, 1);
    }

    #[test]
    fn parse_grade_invalid_returns_none() {
        assert!(parse_grade_response("test", "This skill is okay").is_none());
        assert!(parse_grade_response("test", "GRADE:0 invalid").is_none());
        assert!(parse_grade_response("test", "GRADE:6 too high").is_none());
    }

    #[test]
    fn parse_grade_with_preamble() {
        let response = "Looking at this skill...\n\nGRADE:3 Decent but could be more specific.";
        let review = parse_grade_response("test", response).unwrap();
        assert_eq!(review.grade, 3);
    }

}
