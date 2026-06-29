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
}

impl<T: AgentTurn> AgentResponder<T> {
    pub fn new(turn: T) -> Self {
        Self {
            turn,
            last_follow_on: FollowOn::BackToLlm,
        }
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

#[async_trait]
impl<T: AgentTurn> LlmResponder for AgentResponder<T> {
    async fn respond(&mut self, prompt_text: &str) -> TransportResult<String> {
        let reply = self.turn.run_single(prompt_text).await?;
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
