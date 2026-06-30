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
    briefed: bool,
}

impl<T: AgentTurn> AgentResponder<T> {
    pub fn new(turn: T) -> Self {
        Self {
            turn,
            last_follow_on: FollowOn::BackToLlm,
            locale: default_locale(),
            briefed: false,
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

/// Briefing sent once at the start of the conversation. After this turn the
/// guide answers from conversation history, so later prompts carry only the
/// field text. This is what makes the walk a single `zeroclaw agent` exchange
/// with memory rather than a sequence of stateless one-shot questions.
const GUIDE_BRIEFING: &str = "You are guiding an operator through ZeroClaw setup, one field at a time. \
I will send each field in turn. Reply with ONLY the value for that field and nothing else: \
no explanation, no preamble, no tool use, no file lookups. \
For a yes/no field reply `yes` or `no`. For a number reply only the number. \
For a choice reply exactly one offered option. For free text reply the value on one line. \
If a field is optional and has no sensible value, reply `none`. \
Use what you already chose for earlier fields to stay consistent. Answer the first field now.\n\n";

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
        let briefing = if self.briefed { "" } else { GUIDE_BRIEFING };
        self.briefed = true;
        let directive = locale_directive(&self.locale);
        let message = format!("{briefing}{directive}{prompt_text}");
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
    async fn responder_briefs_once_then_sends_only_the_field() {
        let mut responder = AgentResponder::new(ScriptedTurn::new(vec!["yes", "no"]));
        responder.respond("First field?").await.unwrap();
        responder.respond("Second field?").await.unwrap();
        let seen = &responder.turn.seen;
        assert!(
            seen[0].contains("ZeroClaw setup") && seen[0].ends_with("First field?"),
            "the first turn carries the briefing, got: {}",
            seen[0]
        );
        assert_eq!(
            seen[1], "Second field?",
            "later turns carry only the field; continuity comes from conversation history"
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
