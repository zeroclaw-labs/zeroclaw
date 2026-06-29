use crate::response_type::{PromptSigil, ResponseType, ResponseValue};
use async_trait::async_trait;

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

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("transport closed before the prompt was answered")]
    Closed,
    #[error("response type mismatch: expected {expected:?}")]
    ResponseTypeMismatch { expected: ResponseType },
}

pub type TransportResult<T> = Result<T, TransportError>;

#[async_trait]
pub trait FlowTransport: Send {
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue>;
    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfiguredItem {
    pub layer: String,
    pub instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Completed { configured: Vec<ConfiguredItem> },
    Cancelled,
    Failed {
        layer: String,
        instance: String,
        reason: String,
    },
}

impl Outcome {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Outcome::Cancelled => "cancelled".to_string(),
            Outcome::Completed { configured } => {
                let summary = configured
                    .iter()
                    .map(|item| format!("{}:{}", item.layer, item.instance))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("completed: {summary}")
            }
            Outcome::Failed {
                layer,
                instance,
                reason,
            } => format!("failed: {layer}:{instance}: {reason}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_type::{ResponseType, SecretValue};
    use std::collections::VecDeque;

    struct RoutingTransport {
        scripted: VecDeque<ResponseValue>,
        seen_secret_via_secret_channel: bool,
        seen_secret_in_visible_text: bool,
        emitted: Vec<Outcome>,
    }

    impl RoutingTransport {
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
    impl FlowTransport for RoutingTransport {
        async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
            let answer = self.scripted.pop_front().ok_or(TransportError::Closed)?;
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

    async fn drive(transport: &mut dyn FlowTransport) -> Vec<ResponseValue> {
        let sequence = [
            Prompt::new("Enable telemetry?", ResponseType::YesNo),
            Prompt::new("API key", ResponseType::Secret),
            Prompt::new("Agent name", ResponseType::FreeformText),
        ];
        let mut answers = Vec::new();
        for prompt in sequence {
            answers.push(transport.ask(&prompt).await.unwrap());
        }
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
    async fn both_transports_drive_identical_answers() {
        let mut first = RoutingTransport::new(scripted_answers());
        let mut second = RoutingTransport::new(scripted_answers());
        assert_eq!(drive(&mut first).await, drive(&mut second).await);
    }

    #[tokio::test]
    async fn secret_prompt_routes_through_secret_channel() {
        let mut transport = RoutingTransport::new(scripted_answers());
        drive(&mut transport).await;
        assert!(transport.seen_secret_via_secret_channel);
        assert!(!transport.seen_secret_in_visible_text);
    }

    #[test]
    fn secret_prompt_uses_hash_sigil() {
        let prompt = Prompt::new("API key", ResponseType::Secret);
        assert_eq!(prompt.sigil().as_str(), "#");
        assert!(prompt.routes_secret());
    }

    #[test]
    fn visible_prompt_uses_angle_sigil() {
        let prompt = Prompt::new("Agent name", ResponseType::FreeformText);
        assert_eq!(prompt.sigil().as_str(), ">");
        assert!(!prompt.routes_secret());
    }
}
