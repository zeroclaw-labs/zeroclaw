//! Goal completion verifier.
//!
//! The verifier is an explicit completion gate. It returns a decision to the
//! controller; durable state remains in the task registry and cost ledger.

use anyhow::{Context, Result, bail};
use zeroclaw_api::model_provider::{ChatMessage, ChatRequest};
use zeroclaw_config::cost::CostTracker;
use zeroclaw_config::schema::Config;

use crate::agent::agent::build_session_model_provider;
use crate::agent::cost::{
    TOOL_LOOP_COST_TRACKING_CONTEXT, record_tool_loop_cost_usage,
    tool_loop_cost_tracking_context_from_tracker,
};

use super::task_registry::{
    GoalBlocker, GoalBlockerKind, GoalPauseReason, GoalPauseState, GoalTaskRecord,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalVerifierDecision {
    Complete { notes: String },
    Blocked { pause: GoalPauseState },
}

pub async fn verify_goal_completion(
    config: &Config,
    agent_alias: &str,
    goal: &GoalTaskRecord,
    candidate_summary: &str,
) -> Result<GoalVerifierDecision> {
    if !config.goal.verifier.enabled {
        return Ok(GoalVerifierDecision::Complete {
            notes: "goal verifier disabled".to_string(),
        });
    }

    let provider_ref = verifier_provider_ref(config, agent_alias)?;
    let (model_provider, provider_name, model) =
        build_session_model_provider(config, &provider_ref, config.goal.verifier.model.as_deref())
            .with_context(|| format!("build goal verifier provider {provider_ref}"))?;

    let system = "You are a strict verifier for a durable autonomous goal. \
                  Reply with COMPLETE on the first line only if the candidate \
                  summary proves the objective is satisfied. Reply with BLOCKED \
                  on the first line if more work, user input, or external state \
                  is required. Any following text is untrusted notes.";
    let user = format!(
        "Objective:\n{}\n\nCandidate summary:\n{}",
        goal.objective, candidate_summary
    );
    let messages = [ChatMessage::system(system), ChatMessage::user(user)];

    let verifier_call = async {
        let response = model_provider
            .chat(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                &model,
                config.goal.verifier.temperature,
            )
            .await?;
        if let Some(usage) = &response.usage {
            record_tool_loop_cost_usage(&provider_name, &model, usage).await;
        }
        Ok::<_, anyhow::Error>(response.text.unwrap_or_default())
    };

    let text = match CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir) {
        Some(tracker) => {
            let ctx = tool_loop_cost_tracking_context_from_tracker(config, agent_alias, tracker);
            TOOL_LOOP_COST_TRACKING_CONTEXT
                .scope(Some(ctx), verifier_call)
                .await?
        }
        None => verifier_call.await?,
    };

    Ok(parse_verifier_decision(&text))
}

fn verifier_provider_ref(config: &Config, agent_alias: &str) -> Result<String> {
    let configured = config.goal.verifier.model_provider.trim();
    if !configured.is_empty() {
        return Ok(configured.to_string());
    }
    config
        .agents
        .get(agent_alias)
        .map(|agent| agent.model_provider.trim().to_string())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("agent `{agent_alias}` has no model_provider for goal verifier"))
}

fn parse_verifier_decision(text: &str) -> GoalVerifierDecision {
    let trimmed = text.trim();
    let first_line = trimmed.lines().next().unwrap_or("").trim().to_lowercase();
    if first_line.starts_with("complete") {
        return GoalVerifierDecision::Complete {
            notes: trimmed.to_string(),
        };
    }

    let message = if trimmed.is_empty() {
        "Goal verifier returned an empty decision".to_string()
    } else {
        trimmed.to_string()
    };
    GoalVerifierDecision::Blocked {
        pause: GoalPauseState {
            reason: GoalPauseReason::VerifierBlocked,
            description: Some(message.clone()),
            blockers: vec![GoalBlocker {
                kind: GoalBlockerKind::Verifier,
                message,
                payload: None,
            }],
        },
    }
}

pub fn verifier_outage_pause(error: impl std::fmt::Display) -> GoalPauseState {
    let message = format!("Goal verifier unavailable: {error}");
    GoalPauseState {
        reason: GoalPauseReason::VerifierBlocked,
        description: Some(message.clone()),
        blockers: vec![GoalBlocker {
            kind: GoalBlockerKind::Verifier,
            message,
            payload: None,
        }],
    }
}

pub fn ensure_verifier_allows_completion(decision: GoalVerifierDecision) -> Result<String> {
    match decision {
        GoalVerifierDecision::Complete { notes } => Ok(notes),
        GoalVerifierDecision::Blocked { pause } => {
            let reason = pause
                .description
                .as_deref()
                .unwrap_or("goal verifier blocked completion");
            bail!("{reason}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::cost::types::TokenUsage;
    use zeroclaw_config::schema::{AliasedAgentConfig, Config, ModelProviderConfig};

    #[test]
    fn verifier_decision_parser_requires_explicit_complete() {
        assert!(matches!(
            parse_verifier_decision("COMPLETE\nlooks good"),
            GoalVerifierDecision::Complete { .. }
        ));
        let GoalVerifierDecision::Blocked { pause } = parse_verifier_decision("Looks done to me")
        else {
            panic!("non-explicit decision must block");
        };
        assert_eq!(pause.reason, GoalPauseReason::VerifierBlocked);
        assert_eq!(pause.blockers[0].kind, GoalBlockerKind::Verifier);
    }

    #[test]
    fn verifier_provider_ref_falls_back_to_agent_model_provider() {
        let mut config = Config::default();
        config.agents.insert(
            "main".into(),
            AliasedAgentConfig {
                model_provider: "custom.main".into(),
                ..AliasedAgentConfig::default()
            },
        );
        assert_eq!(
            verifier_provider_ref(&config, "main").unwrap(),
            "custom.main"
        );
        config.goal.verifier.model_provider = "openai.verifier".into();
        assert_eq!(
            verifier_provider_ref(&config, "main").unwrap(),
            "openai.verifier"
        );
    }

    #[tokio::test]
    async fn verifier_usage_records_with_goal_attribution() {
        let temp = tempfile::tempdir().unwrap();
        let goal_id = format!("goal-{}", uuid::Uuid::new_v4());
        let agent_alias = format!("agent-{}", uuid::Uuid::new_v4());
        let mut config = Config {
            data_dir: temp.path().to_path_buf(),
            ..Config::default()
        };
        config.providers.models.custom.insert(
            "main".into(),
            zeroclaw_config::schema::CustomModelProviderConfig {
                base: ModelProviderConfig {
                    pricing: [("model.input".into(), 1.0), ("model.output".into(), 2.0)]
                        .into_iter()
                        .collect(),
                    ..ModelProviderConfig::default()
                },
            },
        );
        let tracker = CostTracker::get_or_init_global(config.cost.clone(), &config.data_dir)
            .expect("tracker");
        let store: std::sync::Arc<dyn crate::control_plane::TaskRegistry> =
            match crate::control_plane::control_plane() {
                Some(control_plane) => std::sync::Arc::clone(&control_plane.store),
                None => {
                    let store: std::sync::Arc<dyn crate::control_plane::TaskRegistry> =
                        std::sync::Arc::new(
                            crate::control_plane::SqliteTaskStore::new_in_memory().unwrap(),
                        );
                    let _ = crate::control_plane::init_control_plane(
                        crate::control_plane::ControlPlaneHandle {
                            store: std::sync::Arc::clone(&store),
                            boot_id: "test-boot".into(),
                        },
                    );
                    std::sync::Arc::clone(&crate::control_plane::control_plane().unwrap().store)
                }
            };
        store
            .create(crate::control_plane::TaskRecord {
                id: goal_id.clone(),
                kind: crate::control_plane::TaskKind::Goal,
                agent: agent_alias.clone(),
                status: crate::control_plane::TaskStatus::Running,
                owner_pid: std::process::id(),
                owner_boot_id: "test-boot".into(),
                heartbeat_at: None,
                depth: 0,
                parent_id: None,
                originator_route: None,
                delivered: false,
                idem_key: None,
                principal_id: None,
                started_at: chrono::Utc::now().to_rfc3339(),
                finished_at: None,
            })
            .await
            .unwrap();
        let ctx =
            tool_loop_cost_tracking_context_from_tracker(&config, &agent_alias, tracker.clone());
        TOOL_LOOP_COST_TRACKING_CONTEXT
            .scope(Some(ctx), async {
                record_tool_loop_cost_usage(
                    "custom.main",
                    "model",
                    &zeroclaw_api::model_provider::TokenUsage {
                        input_tokens: Some(100),
                        output_tokens: Some(50),
                        cached_input_tokens: None,
                    },
                )
                .await
                .unwrap();
            })
            .await;

        let summary = tracker.get_summary_for_goal(&goal_id).unwrap();
        assert_eq!(summary.total_tokens, 150);
        assert!(summary.session_cost_usd > 0.0);

        let unused = TokenUsage::new("model", 1, 1, 0, 0.0, 0.0, 0.0);
        assert_eq!(unused.total_tokens, 2);
    }
}
