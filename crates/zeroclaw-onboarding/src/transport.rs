use async_trait::async_trait;
use zeroclaw_runtime::response_type::{PromptSigil, ResponseType, ResponseValue};

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed before the prompt was answered")]
    Closed,
    #[error("response type mismatch: expected {expected:?}")]
    ResponseTypeMismatch { expected: ResponseType },
}

pub type TransportResult<T> = Result<T, TransportError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub text: String,
    pub response_type: ResponseType,
}

impl Prompt {
    #[must_use]
    pub fn new(text: impl Into<String>, response_type: ResponseType) -> Self {
        Self {
            text: text.into(),
            response_type,
        }
    }

    #[must_use]
    pub fn sigil(&self) -> PromptSigil {
        self.response_type.prompt_sigil()
    }

    #[must_use]
    pub fn routes_secret(&self) -> bool {
        matches!(self.response_type, ResponseType::Secret)
    }
}

#[async_trait]
pub trait OnboardingTransport: Send {
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue>;
    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    ValidationError { field: String, message: String },
    AgentCreated { alias: String },
    Cancelled,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use zeroclaw_runtime::response_type::SecretValue;

    struct ScriptedTransport {
        scripted: VecDeque<ResponseValue>,
        seen_secret_via_secret_channel: bool,
        seen_secret_in_visible_text: bool,
        emitted: Vec<Outcome>,
    }

    impl ScriptedTransport {
        fn new(scripted: Vec<ResponseValue>) -> Self {
            Self {
                scripted: scripted.into(),
                seen_secret_via_secret_channel: false,
                seen_secret_in_visible_text: false,
                emitted: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl OnboardingTransport for ScriptedTransport {
        async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
            let answer = self
                .scripted
                .pop_front()
                .ok_or(TransportError::Closed)?;
            if prompt.routes_secret() {
                if let ResponseValue::Secret(_) = &answer {
                    self.seen_secret_via_secret_channel = true;
                } else {
                    self.seen_secret_in_visible_text = true;
                }
            }
            Ok(answer)
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    async fn drive(transport: &mut dyn OnboardingTransport) -> Vec<ResponseValue> {
        let sequence = [
            Prompt::new("Enable telemetry?", ResponseType::YesNo),
            Prompt::new("API key", ResponseType::Secret),
            Prompt::new("Agent name", ResponseType::FreeformText),
        ];
        let mut answers = Vec::new();
        for prompt in sequence {
            answers.push(transport.ask(&prompt).await.unwrap());
        }
        transport
            .emit(&Outcome::AgentCreated {
                alias: "scout".into(),
            })
            .await
            .unwrap();
        answers
    }

    fn scripted_answers() -> Vec<ResponseValue> {
        vec![
            ResponseValue::YesNo(true),
            ResponseValue::Secret(SecretValue::new("sk-live-xyz".into())),
            ResponseValue::FreeformText("scout".into()),
        ]
    }

    #[tokio::test]
    async fn both_transports_drive_identical_flow() {
        let mut first = ScriptedTransport::new(scripted_answers());
        let mut second = ScriptedTransport::new(scripted_answers());

        let first_answers = drive(&mut first).await;
        let second_answers = drive(&mut second).await;

        assert_eq!(first_answers, second_answers);
        assert_eq!(first.emitted, second.emitted);
        assert_eq!(
            first.emitted,
            vec![Outcome::AgentCreated {
                alias: "scout".into()
            }]
        );
    }

    #[tokio::test]
    async fn secret_prompt_routes_through_secret_channel() {
        let mut transport = ScriptedTransport::new(scripted_answers());
        drive(&mut transport).await;
        assert!(transport.seen_secret_via_secret_channel);
        assert!(!transport.seen_secret_in_visible_text);
    }

    #[test]
    fn secret_prompt_uses_hash_sigil() {
        let prompt = Prompt::new("API key", ResponseType::Secret);
        assert_eq!(prompt.sigil(), PromptSigil::Secret);
        assert!(prompt.routes_secret());
    }

    #[test]
    fn visible_prompt_uses_angle_sigil() {
        let prompt = Prompt::new("Agent name", ResponseType::FreeformText);
        assert_eq!(prompt.sigil(), PromptSigil::Visible);
        assert!(!prompt.routes_secret());
    }
}
