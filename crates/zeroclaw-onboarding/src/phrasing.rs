use async_trait::async_trait;
use zeroclaw_runtime::flow::{Spec, TransportResult};
use zeroclaw_runtime::response_type::AskKind;

use crate::agent_responder::AgentTurn;

pub struct FieldPhrasingContext {
    pub prop_path: String,
    pub description: String,
    pub ask_kind: AskKind,
}

#[async_trait]
pub trait PromptPhraser: Send {
    async fn phrase(&mut self, context: &FieldPhrasingContext) -> TransportResult<String>;
}

pub struct DescriptionPhraser;

#[async_trait]
impl PromptPhraser for DescriptionPhraser {
    async fn phrase(&mut self, context: &FieldPhrasingContext) -> TransportResult<String> {
        if context.description.is_empty() {
            Ok(context.prop_path.clone())
        } else {
            Ok(context.description.clone())
        }
    }
}

pub struct AgentPhraser<T: AgentTurn> {
    turn: T,
}

impl<T: AgentTurn> AgentPhraser<T> {
    pub fn new(turn: T) -> Self {
        Self { turn }
    }

    fn instruction(context: &FieldPhrasingContext) -> String {
        format!(
            "Rewrite this configuration field as a single short imperative \
             instruction to the operator, like \"Provide the Discord bot token\" \
             or \"Describe the agent\". Reply with only the instruction, no \
             quotes, no trailing punctuation.\nField path: {}\nField docs: {}\n\
             Answer kind: {}",
            context.prop_path, context.description, context.ask_kind
        )
    }
}

#[async_trait]
impl<T: AgentTurn> PromptPhraser for AgentPhraser<T> {
    async fn phrase(&mut self, context: &FieldPhrasingContext) -> TransportResult<String> {
        let reply = self.turn.run_single(&Self::instruction(context)).await?;
        let cleaned = reply.trim().trim_matches('"').trim().to_string();
        if cleaned.is_empty() {
            Ok(context.description.clone())
        } else {
            Ok(cleaned)
        }
    }
}

pub async fn phrase_spec(spec: &mut Spec, phraser: &mut dyn PromptPhraser) -> TransportResult<()> {
    for node in spec.nodes.values_mut() {
        let context = FieldPhrasingContext {
            prop_path: node.prop.clone(),
            description: node.prompt.text.clone(),
            ask_kind: node.prompt.response_type.ask_kind(),
        };
        let phrased = phraser.phrase(&context).await?;
        node.prompt.text = phrased;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use zeroclaw_runtime::flow::TransportError;
    use zeroclaw_runtime::response_type::ResponseType;

    struct ScriptedTurn {
        replies: VecDeque<String>,
        prompts: Vec<String>,
    }

    impl ScriptedTurn {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                prompts: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl AgentTurn for ScriptedTurn {
        async fn run_single(&mut self, message: &str) -> TransportResult<String> {
            self.prompts.push(message.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    fn context(path: &str, docs: &str) -> FieldPhrasingContext {
        FieldPhrasingContext {
            prop_path: path.to_string(),
            description: docs.to_string(),
            ask_kind: ResponseType::Secret.ask_kind(),
        }
    }

    #[tokio::test]
    async fn description_phraser_uses_docs_when_present() {
        let mut phraser = DescriptionPhraser;
        let phrased = phraser
            .phrase(&context("channels.discord.token", "The Discord bot token."))
            .await
            .unwrap();
        assert_eq!(phrased, "The Discord bot token.");
    }

    #[tokio::test]
    async fn description_phraser_falls_back_to_path_when_no_docs() {
        let mut phraser = DescriptionPhraser;
        let phrased = phraser
            .phrase(&context("channels.discord.token", ""))
            .await
            .unwrap();
        assert_eq!(phrased, "channels.discord.token");
    }

    #[tokio::test]
    async fn agent_phraser_returns_a_cleaned_imperative() {
        let mut phraser = AgentPhraser::new(ScriptedTurn::new(vec![
            "  \"Provide the Discord bot token\"  ",
        ]));
        let phrased = phraser
            .phrase(&context("channels.discord.token", "The Discord bot token."))
            .await
            .unwrap();
        assert_eq!(phrased, "Provide the Discord bot token");
    }

    #[tokio::test]
    async fn agent_phraser_sends_the_field_metadata_to_the_agent() {
        let mut phraser = AgentPhraser::new(ScriptedTurn::new(vec!["Provide the token"]));
        phraser
            .phrase(&context("channels.discord.token", "The Discord bot token."))
            .await
            .unwrap();
        let turn = phraser.turn;
        assert_eq!(turn.prompts.len(), 1);
        assert!(turn.prompts[0].contains("channels.discord.token"));
        assert!(turn.prompts[0].contains("The Discord bot token."));
    }

    #[tokio::test]
    async fn agent_phraser_falls_back_to_docs_on_empty_reply() {
        let mut phraser = AgentPhraser::new(ScriptedTurn::new(vec!["   "]));
        let phrased = phraser
            .phrase(&context("channels.discord.token", "The Discord bot token."))
            .await
            .unwrap();
        assert_eq!(phrased, "The Discord bot token.");
    }

    #[tokio::test]
    async fn phrase_spec_rewrites_every_node_prompt_text() {
        use crate::spec_builder::build_spec;
        use zeroclaw_config::schema::{Config, MatrixConfig};
        use zeroclaw_runtime::flow::Outcome;

        let mut config = Config::default();
        config
            .channels
            .matrix
            .insert("home".to_string(), MatrixConfig::default());
        let mut spec = build_spec(
            config.prop_fields(),
            "channels.matrix.home",
            "channel",
            "home",
            Outcome::Cancelled,
        )
        .expect("matrix yields a spec");

        struct ConstantPhraser;
        #[async_trait]
        impl PromptPhraser for ConstantPhraser {
            async fn phrase(&mut self, _context: &FieldPhrasingContext) -> TransportResult<String> {
                Ok("PHRASED".to_string())
            }
        }

        phrase_spec(&mut spec, &mut ConstantPhraser).await.unwrap();
        assert!(
            spec.nodes
                .values()
                .all(|node| node.prompt.text == "PHRASED"),
            "every node prompt should have been rephrased"
        );
    }
}
