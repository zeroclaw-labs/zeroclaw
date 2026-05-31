use async_trait::async_trait;
use std::sync::Arc;

use crate::hooks::traits::HookHandler;
use crate::observability::{Observer, ObserverEvent};
use crate::skills::store::SkillStore;

use super::background_llm::{BackgroundLlmConfig, background_llm_call};

use daemonclaw_api::agent::TurnResult;

const DEVIATION_PROMPT_TEMPLATE: &str = r#"You are a skill deviation detector. Given an active skill's instructions and the turn's actual actions, classify the relationship.

Respond with exactly one of:
- FOLLOWED — the agent followed the skill's procedure closely
- DEVIATED — the agent diverged from the skill's procedure in a meaningful way
- UNRELATED — the skill was not relevant to this turn's work

## Active Skill Instructions

{{skill_body}}

## Turn Actions

User message: {{user_message}}

Tool calls ({{tool_call_count}} total):
{{tool_calls_summary}}

Classification:"#;

const PATCH_PROMPT_TEMPLATE: &str = r#"You are a skill updater. The agent deviated from the following skill during a turn. Update the skill body to incorporate the deviation as an improvement.

Keep the same structure (## When to Use, ## Procedure, ## Notes) but update the content to reflect what the agent actually did. Be concise.

## Current Skill Body

{{skill_body}}

## What Actually Happened

User message: {{user_message}}

Tool calls:
{{tool_calls_summary}}

## Updated Skill Body (respond with ONLY the updated markdown, no fences):"#;

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeviationClass {
    Followed,
    Deviated,
    Unrelated,
}

/// Hook that detects when the agent deviates from an active skill and patches
/// the skill to incorporate the deviation.
pub struct SkillPatcherHook {
    store: Arc<SkillStore>,
    observer: Arc<dyn Observer>,
    llm_config: BackgroundLlmConfig,
}

impl SkillPatcherHook {
    pub fn new(
        store: Arc<SkillStore>,
        observer: Arc<dyn Observer>,
        llm_config: BackgroundLlmConfig,
    ) -> Self {
        Self {
            store,
            observer,
            llm_config,
        }
    }

    fn tool_calls_summary(result: &TurnResult) -> String {
        result
            .tool_calls
            .iter()
            .take(20)
            .enumerate()
            .map(|(i, tc)| {
                format!(
                    "{}. {} (success={}, {}ms)",
                    i + 1,
                    tc.name,
                    tc.success,
                    tc.duration.as_millis()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn build_deviation_prompt(result: &TurnResult, skill_body: &str) -> String {
        let summary = Self::tool_calls_summary(result);
        DEVIATION_PROMPT_TEMPLATE
            .replace("{{skill_body}}", skill_body)
            .replace("{{user_message}}", &result.user_message)
            .replace("{{tool_call_count}}", &result.tool_call_count.to_string())
            .replace("{{tool_calls_summary}}", &summary)
    }

    fn build_patch_prompt(result: &TurnResult, skill_body: &str) -> String {
        let summary = Self::tool_calls_summary(result);
        PATCH_PROMPT_TEMPLATE
            .replace("{{skill_body}}", skill_body)
            .replace("{{user_message}}", &result.user_message)
            .replace("{{tool_calls_summary}}", &summary)
    }

    fn parse_deviation(response: &str) -> DeviationClass {
        let trimmed = response.trim().to_uppercase();
        if trimmed.starts_with("FOLLOWED") {
            DeviationClass::Followed
        } else if trimmed.starts_with("DEVIATED") {
            DeviationClass::Deviated
        } else if trimmed.starts_with("UNRELATED") {
            DeviationClass::Unrelated
        } else {
            tracing::debug!(target: "skill_patcher", response = %trimmed, "unparseable deviation response, treating as FOLLOWED");
            DeviationClass::Followed
        }
    }
}

#[async_trait]
impl HookHandler for SkillPatcherHook {
    fn name(&self) -> &str {
        "skill-patcher"
    }

    fn priority(&self) -> i32 {
        -110
    }

    async fn on_turn_complete(&self, result: &TurnResult) -> crate::hooks::traits::TurnCompleteAction {
        use crate::hooks::traits::TurnCompleteAction;
        let cont = TurnCompleteAction::Continue;

        let skill_name = match &result.active_skill {
            Some(name) => name.clone(),
            None => return cont,
        };

        let skill = match self.store.get_agent(&skill_name) {
            Ok(Some(s)) => s,
            _ => return cont,
        };

        if skill.meta().pinned {
            tracing::debug!(target: "skill_patcher", name = %skill_name, "skill is pinned, skipping deviation check");
            return cont;
        }

        let deviation_prompt = Self::build_deviation_prompt(result, &skill.body);
        let deviation_response = match background_llm_call(
            &self.llm_config,
            &deviation_prompt,
            Some(&self.observer),
        )
        .await
        {
            Some(r) => r,
            None => return cont,
        };

        let classification = Self::parse_deviation(&deviation_response);
        tracing::debug!(
            target: "skill_patcher",
            name = %skill_name,
            classification = ?classification,
            "deviation detection result"
        );

        if classification != DeviationClass::Deviated {
            return cont;
        }

        let patch_prompt = Self::build_patch_prompt(result, &skill.body);
        let new_body = match background_llm_call(
            &self.llm_config,
            &patch_prompt,
            Some(&self.observer),
        )
        .await
        {
            Some(r) => r,
            None => return cont,
        };

        let new_body = new_body.trim().to_string();
        if new_body.is_empty() {
            return cont;
        }

        let mut updated_fm = skill.frontmatter.clone();
        updated_fm.metadata.version = (updated_fm
            .metadata
            .version
            .parse::<u64>()
            .unwrap_or(1)
            + 1)
        .to_string();
        updated_fm.metadata.updated = Some(chrono::Utc::now().to_rfc3339());

        match self.store.write_agent(&skill_name, &updated_fm, &new_body) {
            Ok(()) => {
                tracing::info!(target: "skill_patcher", name = %skill_name, "patched skill after deviation");
                self.observer.record_event(&ObserverEvent::SkillPatched {
                    skill_name,
                    sections_changed: vec!["body".into()],
                });
            }
            Err(e) => {
                tracing::warn!(target: "skill_patcher", name = %skill_name, "failed to patch skill: {e}");
            }
        }
        cont
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use daemonclaw_api::agent::{ToolCallRecord, TurnOutcome};

    fn make_turn_result(active_skill: Option<String>) -> TurnResult {
        let tool_calls = vec![
            ToolCallRecord {
                name: "shell".into(),
                arguments: serde_json::json!({"cmd": "nginx -t"}),
                result: "syntax ok".into(),
                success: true,
                duration: Duration::from_millis(200),
            },
            ToolCallRecord {
                name: "shell".into(),
                arguments: serde_json::json!({"cmd": "systemctl reload nginx"}),
                result: "ok".into(),
                success: true,
                duration: Duration::from_millis(150),
            },
        ];

        TurnResult {
            user_message: "Reload nginx".into(),
            tool_calls,
            tool_call_count: 2,
            active_skill,
            outcome: TurnOutcome::Success,
            final_response: "Nginx reloaded successfully.".into(),
            turn_number: 3,
        }
    }

    #[test]
    fn parse_deviation_followed() {
        assert_eq!(
            SkillPatcherHook::parse_deviation("FOLLOWED"),
            DeviationClass::Followed
        );
        assert_eq!(
            SkillPatcherHook::parse_deviation("FOLLOWED - agent stuck to the script"),
            DeviationClass::Followed
        );
    }

    #[test]
    fn parse_deviation_deviated() {
        assert_eq!(
            SkillPatcherHook::parse_deviation("DEVIATED"),
            DeviationClass::Deviated
        );
        assert_eq!(
            SkillPatcherHook::parse_deviation("  deviated  \n"),
            DeviationClass::Deviated
        );
    }

    #[test]
    fn parse_deviation_unrelated() {
        assert_eq!(
            SkillPatcherHook::parse_deviation("UNRELATED"),
            DeviationClass::Unrelated
        );
    }

    #[test]
    fn parse_deviation_unknown_defaults_to_followed() {
        assert_eq!(
            SkillPatcherHook::parse_deviation("I think the agent followed the skill"),
            DeviationClass::Followed
        );
    }

    #[test]
    fn build_deviation_prompt_includes_skill_body() {
        let result = make_turn_result(Some("deploy-nginx".into()));
        let prompt =
            SkillPatcherHook::build_deviation_prompt(&result, "## Procedure\n1. Test\n2. Reload");
        assert!(prompt.contains("## Procedure"));
        assert!(prompt.contains("Reload nginx"));
        assert!(prompt.contains("shell"));
    }

    #[test]
    fn build_patch_prompt_includes_context() {
        let result = make_turn_result(Some("deploy-nginx".into()));
        let prompt =
            SkillPatcherHook::build_patch_prompt(&result, "## Procedure\n1. Test\n2. Reload");
        assert!(prompt.contains("## Procedure"));
        assert!(prompt.contains("Reload nginx"));
    }
}
