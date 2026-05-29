use async_trait::async_trait;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::hooks::traits::HookHandler;
use crate::observability::{Observer, ObserverEvent};
use crate::user_model::UserModelStore;

use super::background_llm::{BackgroundLlmConfig, background_llm_call};

use daemonclaw_api::agent::TurnResult;

const DEFAULT_COLD_START_TURN: u64 = 3;
const DEFAULT_UPDATE_CADENCE: u64 = 10;

const COLD_START_PROMPT: &str = r#"You are a user modeling agent. Based on the following conversation turns, build an initial user profile.

Respond with a JSON object (no markdown fences) containing only the fields you can confidently infer:
{
  "communication_style": {
    "verbosity": "terse" | "normal" | "verbose",
    "tone": "casual" | "professional" | "technical"
  },
  "expertise_areas": [
    {"domain": "...", "level": "beginner" | "intermediate" | "advanced" | "expert", "notes": "..."}
  ],
  "preferences": {
    "preferred_tools": ["..."],
    "workflow_notes": ["..."]
  },
  "goals": [
    {"description": "...", "status": "active"}
  ]
}

Omit any fields you cannot infer. Be conservative — only include what the evidence supports.

## Recent Turns

{{turns_summary}}"#;

const UPDATE_PROMPT: &str = r#"You are a user modeling agent. Given the current user model and recent conversation turns, produce a JSON Merge Patch (RFC 7386) to update the model.

Rules:
- Only include fields that should change based on new evidence
- Set a field to null to remove it
- Be conservative — do not remove fields without strong evidence
- Increment the version number

Current model:
{{current_model}}

## Recent Turns

{{turns_summary}}

Respond with a JSON merge patch object (no fences):"#;

/// Hook that updates the user model via dialectic reasoning.
///
/// Fires on `on_turn_complete` with two modes:
/// - Cold-start: at turn COLD_START_TURN, builds initial model
/// - Warm: every UPDATE_CADENCE turns, patches the model
pub struct DialecticHook {
    store: Arc<UserModelStore>,
    observer: Arc<dyn Observer>,
    llm_config: BackgroundLlmConfig,
    workspace_dir: std::path::PathBuf,
    cold_start_turn: u64,
    update_cadence: u64,
    turn_counter: AtomicU64,
    turns_buffer: std::sync::Mutex<Vec<TurnSummary>>,
}

#[derive(Debug, Clone)]
struct TurnSummary {
    user_message: String,
    tool_count: usize,
    response_snippet: String,
}

impl DialecticHook {
    pub fn new(
        store: Arc<UserModelStore>,
        observer: Arc<dyn Observer>,
        llm_config: BackgroundLlmConfig,
        workspace_dir: std::path::PathBuf,
    ) -> Self {
        Self::with_cadence(store, observer, llm_config, workspace_dir, DEFAULT_COLD_START_TURN, DEFAULT_UPDATE_CADENCE)
    }

    pub fn with_cadence(
        store: Arc<UserModelStore>,
        observer: Arc<dyn Observer>,
        llm_config: BackgroundLlmConfig,
        workspace_dir: std::path::PathBuf,
        cold_start_turn: u64,
        update_cadence: u64,
    ) -> Self {
        Self {
            store,
            observer,
            llm_config,
            workspace_dir,
            cold_start_turn,
            update_cadence,
            turn_counter: AtomicU64::new(0),
            turns_buffer: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn format_turns_summary(turns: &[TurnSummary]) -> String {
        turns
            .iter()
            .enumerate()
            .map(|(i, t)| {
                format!(
                    "Turn {}: User said: \"{}\"\n  Tool calls: {}\n  Response: \"{}\"",
                    i + 1,
                    t.user_message,
                    t.tool_count,
                    t.response_snippet
                )
            })
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    fn parse_json_from_response(response: &str) -> Option<serde_json::Value> {
        let trimmed = response.trim();

        let json_start = trimmed.find('{')?;
        let json_end = trimmed.rfind('}')?;
        if json_end <= json_start {
            return None;
        }

        let json_str = &trimmed[json_start..=json_end];
        serde_json::from_str(json_str).ok()
    }
}

#[async_trait]
impl HookHandler for DialecticHook {
    fn name(&self) -> &str {
        "dialectic"
    }

    fn priority(&self) -> i32 {
        -120
    }

    async fn on_turn_complete(&self, result: &TurnResult) -> crate::hooks::traits::TurnCompleteAction {
        let turn_num = self.turn_counter.fetch_add(1, Ordering::Relaxed) + 1;

        let snippet = if result.final_response.len() > 200 {
            format!("{}...", &result.final_response[..200])
        } else {
            result.final_response.clone()
        };

        {
            let mut buf = self.turns_buffer.lock().unwrap();
            buf.push(TurnSummary {
                user_message: result.user_message.clone(),
                tool_count: result.tool_call_count,
                response_snippet: snippet,
            });
            if buf.len() > 20 {
                let excess = buf.len() - 20;
                buf.drain(..excess);
            }
        }

        let should_cold_start = turn_num == self.cold_start_turn;
        let should_update = turn_num > self.cold_start_turn
            && self.update_cadence > 0
            && turn_num % self.update_cadence == 0;

        if should_cold_start {
            self.run_cold_start().await;
        } else if should_update {
            self.run_update().await;
        }
        crate::hooks::traits::TurnCompleteAction::Continue
    }
}

impl DialecticHook {
    async fn run_cold_start(&self) {
        let turns = self.turns_buffer.lock().unwrap().clone();
        if turns.is_empty() {
            return;
        }

        let summary = Self::format_turns_summary(&turns);
        let prompt = COLD_START_PROMPT.replace("{{turns_summary}}", &summary);

        let response =
            match background_llm_call(&self.llm_config, &prompt, Some(&self.observer)).await {
                Some(r) => r,
                None => return,
            };

        let model_json = match Self::parse_json_from_response(&response) {
            Some(v) => v,
            None => {
                tracing::debug!(target: "dialectic", "cold-start response not parseable as JSON");
                return;
            }
        };

        match self.store.patch(&model_json).await {
            Ok(_model) => {
                let fields: Vec<String> = model_json
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                tracing::info!(target: "dialectic", "cold-start user model created");
                self.observer
                    .record_event(&ObserverEvent::UserModelUpdated {
                        fields_changed: fields,
                    });
                if let Err(e) = self.store.write_user_md(&self.workspace_dir).await {
                    tracing::warn!(target: "dialectic", "failed to write USER.md: {e}");
                }
            }
            Err(e) => {
                tracing::warn!(target: "dialectic", "failed to save cold-start model: {e}");
            }
        }
    }

    async fn run_update(&self) {
        let turns = self.turns_buffer.lock().unwrap().clone();
        if turns.is_empty() {
            return;
        }

        let current_model = match self.store.load().await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(target: "dialectic", "failed to load current model: {e}");
                return;
            }
        };

        let model_json = match serde_json::to_string_pretty(&current_model) {
            Ok(j) => j,
            Err(e) => {
                tracing::warn!(target: "dialectic", "failed to serialize model: {e}");
                return;
            }
        };

        let summary = Self::format_turns_summary(&turns);
        let prompt = UPDATE_PROMPT
            .replace("{{current_model}}", &model_json)
            .replace("{{turns_summary}}", &summary);

        let response =
            match background_llm_call(&self.llm_config, &prompt, Some(&self.observer)).await {
                Some(r) => r,
                None => return,
            };

        let patch = match Self::parse_json_from_response(&response) {
            Some(v) => v,
            None => {
                tracing::debug!(target: "dialectic", "update response not parseable as JSON");
                return;
            }
        };

        match self.store.patch(&patch).await {
            Ok(_model) => {
                let fields: Vec<String> = patch
                    .as_object()
                    .map(|m| m.keys().cloned().collect())
                    .unwrap_or_default();
                tracing::info!(target: "dialectic", fields = ?fields, "user model updated");
                self.observer
                    .record_event(&ObserverEvent::UserModelUpdated {
                        fields_changed: fields,
                    });
                if let Err(e) = self.store.write_user_md(&self.workspace_dir).await {
                    tracing::warn!(target: "dialectic", "failed to write USER.md: {e}");
                }
            }
            Err(e) => {
                tracing::warn!(target: "dialectic", "failed to apply model patch: {e}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_turns_summary_basic() {
        let turns = vec![
            TurnSummary {
                user_message: "Deploy nginx".into(),
                tool_count: 5,
                response_snippet: "Done!".into(),
            },
            TurnSummary {
                user_message: "Check logs".into(),
                tool_count: 2,
                response_snippet: "No errors found.".into(),
            },
        ];
        let summary = DialecticHook::format_turns_summary(&turns);
        assert!(summary.contains("Turn 1"));
        assert!(summary.contains("Deploy nginx"));
        assert!(summary.contains("Turn 2"));
        assert!(summary.contains("Check logs"));
    }

    #[test]
    fn parse_json_from_clean_response() {
        let response = "{\"communication_style\": {\"verbosity\": \"terse\"}}";
        let parsed = DialecticHook::parse_json_from_response(response).unwrap();
        assert_eq!(
            parsed["communication_style"]["verbosity"],
            "terse"
        );
    }

    #[test]
    fn parse_json_with_preamble() {
        let response =
            "Based on the turns, here is the model:\n{\"version\": 1}\nThat's it.";
        let parsed = DialecticHook::parse_json_from_response(response).unwrap();
        assert_eq!(parsed["version"], 1);
    }

    #[test]
    fn parse_json_invalid() {
        assert!(DialecticHook::parse_json_from_response("no json here").is_none());
        assert!(DialecticHook::parse_json_from_response("{broken").is_none());
    }
}
