use async_trait::async_trait;
use zeroclaw_runtime::flow::TransportResult;
use zeroclaw_runtime::response_type::FollowOn;

use crate::llm_transport::LlmResponder;

#[async_trait]
pub trait AgentTurn: Send {
    async fn run_single(&mut self, message: &str) -> TransportResult<String>;
}

pub struct InProcessAgentTurn {
    agent: zeroclaw_runtime::agent::Agent,
}

impl InProcessAgentTurn {
    pub fn new(agent: zeroclaw_runtime::agent::Agent) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl AgentTurn for InProcessAgentTurn {
    async fn run_single(&mut self, message: &str) -> TransportResult<String> {
        self.agent.run_single(message).await.map_err(|error| {
            zeroclaw_runtime::flow::TransportError::Agent {
                reason: error.to_string(),
            }
        })
    }
}

pub struct AgentResponder<T: AgentTurn> {
    turn: T,
    last_follow_on: FollowOn,
    locale: String,
}

impl<T: AgentTurn> AgentResponder<T> {
    pub fn new(turn: T) -> Self {
        Self {
            turn,
            last_follow_on: FollowOn::BackToLlm,
            locale: default_locale(),
        }
    }

    #[must_use]
    pub fn with_locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    pub fn last_follow_on(&self) -> &FollowOn {
        &self.last_follow_on
    }

    fn classify(reply: &str) -> FollowOn {
        let trimmed = reply.trim();
        if trimmed.is_empty() {
            FollowOn::BackToLlm
        } else {
            FollowOn::Complete
        }
    }
}

const DEFAULT_LOCALE: &str = "en";

pub(crate) fn default_locale() -> String {
    DEFAULT_LOCALE.to_string()
}

/// A one-line directive instructing the guide to answer in the operator's
/// chosen locale, resolved to the human label from the `locales.toml` registry.
/// Empty for the English default so existing English prompts are unchanged.
pub(crate) fn locale_directive(locale: &str) -> String {
    if locale == DEFAULT_LOCALE {
        return String::new();
    }
    let label = zeroclaw_runtime::i18n::available_locales()
        .iter()
        .find(|option| option.code == locale)
        .map(|option| option.label.clone())
        .unwrap_or_else(|| locale.to_string());
    format!("Respond in {label}.\n")
}

#[async_trait]
impl<T: AgentTurn> LlmResponder for AgentResponder<T> {
    async fn respond(&mut self, prompt_text: &str) -> TransportResult<String> {
        let directive = locale_directive(&self.locale);
        let message = format!("{directive}{prompt_text}");
        let reply = self.turn.run_single(&message).await?;
        self.last_follow_on = Self::classify(&reply);
        Ok(reply)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use zeroclaw_runtime::flow::TransportError;

    struct ScriptedTurn {
        replies: VecDeque<String>,
        seen: Vec<String>,
    }

    impl ScriptedTurn {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                seen: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl AgentTurn for ScriptedTurn {
        async fn run_single(&mut self, message: &str) -> TransportResult<String> {
            self.seen.push(message.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    #[tokio::test]
    async fn responder_forwards_prompt_to_agent_turn_and_returns_reply() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["yes"]));
        let reply = responder.respond("Enable telemetry?").await.unwrap();
        assert_eq!(reply, "yes");
    }

    #[tokio::test]
    async fn responder_prepends_locale_directive_for_a_non_default_locale() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["oui"])).with_locale("fr");
        responder.respond("Enable telemetry?").await.unwrap();
        let label = zeroclaw_runtime::i18n::available_locales()
            .iter()
            .find(|o| o.code == "fr")
            .map(|o| o.label.clone())
            .expect("fr is registered");
        let seen = responder.turn.seen.last().expect("a prompt was sent");
        assert!(
            seen.contains(&label) && seen.contains("Enable telemetry?"),
            "the guide must be told to answer in the chosen locale, got: {seen}"
        );
    }

    #[tokio::test]
    async fn responder_adds_no_directive_for_the_english_default() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["yes"]));
        responder.respond("Enable telemetry?").await.unwrap();
        let seen = responder.turn.seen.last().expect("a prompt was sent");
        assert_eq!(
            seen, "Enable telemetry?",
            "English default must not alter the prompt"
        );
    }

    #[tokio::test]
    async fn a_non_empty_reply_marks_the_turn_complete() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["multi_message"]));
        responder.respond("stream mode?").await.unwrap();
        assert_eq!(responder.last_follow_on(), &FollowOn::Complete);
    }

    #[tokio::test]
    async fn an_empty_reply_routes_back_to_the_llm() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["   "]));
        responder.respond("stream mode?").await.unwrap();
        assert_eq!(responder.last_follow_on(), &FollowOn::BackToLlm);
    }
}
