use async_trait::async_trait;
use std::sync::Arc;

use crate::hooks::traits::HookHandler;
use crate::observability::{Observer, ObserverEvent};
use crate::skills::store::SkillStore;
use crate::skills::types::{AgentSkillFrontmatter, AgentSkillMeta, SkillSource};

use super::background_llm::{BackgroundLlmConfig, background_llm_call};

use daemonclaw_api::agent::TurnResult;

const DEFAULT_MIN_TOOL_CALLS: usize = 5;

const AUTOGEN_PROMPT_TEMPLATE: &str = r#"You are a skill extraction agent. Given the following turn summary, decide whether this work should be saved as a reusable skill.

A turn is worth saving as a skill if:
- It involved a multi-step procedure (5+ tool calls)
- The procedure could plausibly recur (deployment, debugging pattern, data pipeline, etc.)
- The steps form a coherent workflow, not just exploratory browsing

If the turn is NOT worth saving, respond with exactly: NO_SKILL

If the turn IS worth saving, respond with a JSON object (no markdown fences):
{
  "name": "kebab-case-skill-name",
  "description": "One-line description of what this skill does.",
  "body": "Full markdown body with ## When to Use, ## Procedure, and ## Notes sections."
}

## Turn Summary

User message: {{user_message}}

Tool calls ({{tool_call_count}} total):
{{tool_calls_summary}}

Final response (truncated): {{final_response}}
"#;

/// Hook that autonomously creates skills after complex turns.
///
/// Fires on `on_turn_complete` when:
/// - `tool_call_count >= MIN_TOOL_CALLS`
/// - No skill was already active (avoid creating skills from skill-guided turns)
///
/// Makes a background LLM call to decide whether the turn should become a skill.
pub struct SkillAutogenHook {
    store: Arc<SkillStore>,
    observer: Arc<dyn Observer>,
    llm_config: BackgroundLlmConfig,
    min_tool_calls: usize,
}

impl SkillAutogenHook {
    pub fn new(
        store: Arc<SkillStore>,
        observer: Arc<dyn Observer>,
        llm_config: BackgroundLlmConfig,
    ) -> Self {
        Self::with_threshold(store, observer, llm_config, DEFAULT_MIN_TOOL_CALLS)
    }

    pub fn with_threshold(
        store: Arc<SkillStore>,
        observer: Arc<dyn Observer>,
        llm_config: BackgroundLlmConfig,
        min_tool_calls: usize,
    ) -> Self {
        Self {
            store,
            observer,
            llm_config,
            min_tool_calls,
        }
    }

    fn build_prompt(result: &TurnResult) -> String {
        let tool_calls_summary: String = result
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
            .join("\n");

        let final_response = if result.final_response.len() > 500 {
            format!("{}...", &result.final_response[..500])
        } else {
            result.final_response.clone()
        };

        AUTOGEN_PROMPT_TEMPLATE
            .replace("{{user_message}}", &result.user_message)
            .replace(
                "{{tool_call_count}}",
                &result.tool_call_count.to_string(),
            )
            .replace("{{tool_calls_summary}}", &tool_calls_summary)
            .replace("{{final_response}}", &final_response)
    }

    fn parse_response(text: &str) -> Option<(String, String, String)> {
        let trimmed = text.trim();
        if trimmed == "NO_SKILL" || trimmed.starts_with("NO_SKILL") {
            return None;
        }

        let json_start = trimmed.find('{')?;
        let json_end = trimmed.rfind('}')?;
        if json_end <= json_start {
            return None;
        }

        let json_str = &trimmed[json_start..=json_end];
        let parsed: serde_json::Value = serde_json::from_str(json_str).ok()?;

        let name = parsed.get("name")?.as_str()?.to_string();
        let description = parsed.get("description")?.as_str()?.to_string();
        let body = parsed.get("body")?.as_str()?.to_string();

        if name.is_empty() || description.is_empty() || body.is_empty() {
            return None;
        }

        Some((name, description, body))
    }
}

#[async_trait]
impl HookHandler for SkillAutogenHook {
    fn name(&self) -> &str {
        "skill-autogen"
    }

    fn priority(&self) -> i32 {
        -100
    }

    async fn on_turn_complete(&self, result: &TurnResult) -> crate::hooks::traits::TurnCompleteAction {
        if result.turn_source.is_automated() {
            return crate::hooks::traits::TurnCompleteAction::Continue;
        }

        if result.tool_call_count < self.min_tool_calls {
            return crate::hooks::traits::TurnCompleteAction::Continue;
        }

        if result.active_skill.is_some() {
            return crate::hooks::traits::TurnCompleteAction::Continue;
        }

        let prompt = Self::build_prompt(result);

        let response = match background_llm_call(&self.llm_config, &prompt, Some(&self.observer))
            .await
        {
            Some(r) => r,
            None => return crate::hooks::traits::TurnCompleteAction::Continue,
        };

        let (name, description, body) = match Self::parse_response(&response) {
            Some(parsed) => parsed,
            None => {
                tracing::debug!(target: "skill_autogen", "LLM decided no skill needed or response unparseable");
                return crate::hooks::traits::TurnCompleteAction::Continue;
            }
        };

        if crate::skills::types::validate_skill_name(&name).is_err() {
            tracing::warn!(target: "skill_autogen", name = %name, "LLM proposed invalid skill name");
            return crate::hooks::traits::TurnCompleteAction::Continue;
        }

        if self.store.get(&name).ok().flatten().is_some() {
            tracing::debug!(target: "skill_autogen", name = %name, "skill already exists, skipping autogen");
            return crate::hooks::traits::TurnCompleteAction::Continue;
        }

        let frontmatter = AgentSkillFrontmatter {
            name: name.clone(),
            description,
            license: None,
            metadata: AgentSkillMeta {
                source: SkillSource::Autonomous,
                created: Some(chrono::Utc::now().to_rfc3339()),
                updated: Some(chrono::Utc::now().to_rfc3339()),
                ..AgentSkillMeta::default()
            },
        };

        match self.store.create(&frontmatter, &body) {
            Ok(_path) => {
                tracing::info!(target: "skill_autogen", name = %name, "autonomously created skill");
                self.observer
                    .record_event(&ObserverEvent::SkillCreated { skill_name: name });
            }
            Err(e) => {
                tracing::warn!(target: "skill_autogen", name = %name, "failed to create skill: {e}");
            }
        }
        crate::hooks::traits::TurnCompleteAction::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use daemonclaw_api::agent::{ToolCallRecord, TurnOutcome};

    fn make_turn_result(tool_count: usize, active_skill: Option<String>) -> TurnResult {
        let tool_calls: Vec<ToolCallRecord> = (0..tool_count)
            .map(|i| ToolCallRecord {
                name: format!("tool_{i}"),
                arguments: serde_json::json!({}),
                result: "ok".into(),
                success: true,
                duration: Duration::from_millis(100),
            })
            .collect();

        TurnResult {
            user_message: "Deploy the nginx config and reload".into(),
            tool_calls,
            tool_call_count: tool_count,
            active_skill,
            turn_source: daemonclaw_api::agent::TurnSource::Channel,
            outcome: TurnOutcome::Success,
            final_response: "Done! Nginx config deployed and reloaded.".into(),
            turn_number: 1,
        }
    }

    #[test]
    fn build_prompt_includes_tool_calls() {
        let result = make_turn_result(6, None);
        let prompt = SkillAutogenHook::build_prompt(&result);
        assert!(prompt.contains("Deploy the nginx"));
        assert!(prompt.contains("6 total"));
        assert!(prompt.contains("tool_0"));
        assert!(prompt.contains("tool_5"));
    }

    #[test]
    fn parse_response_no_skill() {
        assert!(SkillAutogenHook::parse_response("NO_SKILL").is_none());
        assert!(SkillAutogenHook::parse_response("NO_SKILL\n").is_none());
    }

    #[test]
    fn parse_response_valid_json() {
        let json = "{\"name\": \"deploy-nginx\", \"description\": \"Deploy nginx config and reload\", \"body\": \"When to use: deploying nginx. Procedure: 1. Edit config 2. Reload\"}";
        let (name, desc, body) = SkillAutogenHook::parse_response(json).unwrap();
        assert_eq!(name, "deploy-nginx");
        assert!(desc.contains("nginx"));
        assert!(body.contains("Procedure"));
    }

    #[test]
    fn parse_response_with_markdown_fences() {
        let response = "Here is the skill:\n```json\n{\"name\": \"fix-auth\", \"description\": \"Fix auth issues\", \"body\": \"Steps: 1. Check logs\"}\n```";
        let (name, _, _) = SkillAutogenHook::parse_response(response).unwrap();
        assert_eq!(name, "fix-auth");
    }

    #[test]
    fn parse_response_invalid_json() {
        assert!(SkillAutogenHook::parse_response("not json at all").is_none());
        assert!(SkillAutogenHook::parse_response("{broken").is_none());
    }

    #[test]
    fn parse_response_missing_fields() {
        let json = r#"{"name": "test"}"#;
        assert!(SkillAutogenHook::parse_response(json).is_none());
    }

    fn make_cron_turn_result(tool_count: usize) -> TurnResult {
        let mut result = make_turn_result(tool_count, None);
        result.turn_source = daemonclaw_api::agent::TurnSource::Cron;
        result
    }

    #[tokio::test]
    async fn automated_turn_skipped() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(SkillStore::new(tmp.path()));
        let observer: Arc<dyn crate::observability::Observer> =
            Arc::new(crate::observability::noop::NoopObserver);
        let llm_config = BackgroundLlmConfig {
            provider_name: "test".into(),
            api_key: None,
            model: "test".into(),
            temperature: 0.0,
            runtime_options: Default::default(),
        };
        let hook = SkillAutogenHook::with_threshold(store, observer, llm_config, 1);
        let result = make_cron_turn_result(10);
        let action = hook.on_turn_complete(&result).await;
        assert_eq!(action, crate::hooks::traits::TurnCompleteAction::Continue);
    }

    #[test]
    fn turn_source_automated_flag() {
        use daemonclaw_api::agent::TurnSource;
        assert!(TurnSource::Cron.is_automated());
        assert!(!TurnSource::Cli.is_automated());
        assert!(!TurnSource::Channel.is_automated());
    }
}
