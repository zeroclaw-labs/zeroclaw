use crate::response_type::{PromptSigil, ResponseType, ResponseValue};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Localizable {
    pub message_id: String,
    pub args: Vec<(String, String)>,
}

impl Localizable {
    #[must_use]
    pub fn new(message_id: impl Into<String>) -> Self {
        Self {
            message_id: message_id.into(),
            args: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_arg(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.args.push((name.into(), value.into()));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Prompt {
    pub text: String,
    pub response_type: ResponseType,
    pub message: Option<Localizable>,
    /// Seed content for an interactive editor session. When set on a freeform
    /// prompt, an interactive transport may open `$EDITOR` pre-filled with this
    /// text instead of reading a single line, so an operator can author a
    /// multi-line personality file. `None` keeps the plain line-read behavior.
    pub editor_seed: Option<String>,
    /// Whether the field may be left unset. When true, a transport may answer
    /// with a skip sentinel (empty / `none` / `skip`) and the walk leaves the
    /// property unwritten instead of looping to re-ask. Optional `Option<_>`
    /// and optional-list fields would otherwise re-prompt forever in a
    /// non-interactive walk because there is no value that means "no value".
    pub optional: bool,
}

impl Prompt {
    #[must_use]
    pub fn new(text: impl Into<String>, response_type: ResponseType) -> Self {
        Self {
            text: text.into(),
            response_type,
            message: None,
            editor_seed: None,
            optional: false,
        }
    }

    #[must_use]
    pub fn with_message(mut self, message: Localizable) -> Self {
        self.message = Some(message);
        self
    }

    #[must_use]
    pub fn with_editor_seed(mut self, seed: impl Into<String>) -> Self {
        self.editor_seed = Some(seed.into());
        self
    }

    #[must_use]
    pub fn with_optional(mut self, optional: bool) -> Self {
        self.optional = optional;
        self
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
    #[error("agent turn failed: {reason}")]
    Agent { reason: String },
}

pub type TransportResult<T> = Result<T, TransportError>;

#[async_trait]
pub trait FlowTransport: Send {
    async fn ask_user(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue>;
    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()>;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfiguredItem {
    pub layer: String,
    pub instance: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Outcome {
    Completed {
        configured: Vec<ConfiguredItem>,
    },
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
        async fn ask_user(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
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
            answers.push(transport.ask_user(&prompt).await.unwrap());
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

    #[test]
    fn prompt_carries_a_serializable_localizable_descriptor() {
        let prompt = Prompt::new("Choose a language", ResponseType::FreeformText)
            .with_message(Localizable::new("onboard-flow-locale-prompt"));
        let descriptor = prompt
            .message
            .as_ref()
            .expect("prompt carries a localizable descriptor");
        assert_eq!(descriptor.message_id, "onboard-flow-locale-prompt");

        let json = serde_json::to_string(&prompt).expect("prompt serializes");
        let restored: Prompt = serde_json::from_str(&json).expect("prompt deserializes");
        assert_eq!(restored, prompt);
    }

    #[test]
    fn localizable_carries_named_args_as_data() {
        let descriptor =
            Localizable::new("onboard-flow-completed").with_arg("items", "channel:home");
        assert_eq!(descriptor.message_id, "onboard-flow-completed");
        assert_eq!(
            descriptor.args,
            vec![("items".to_string(), "channel:home".to_string())]
        );
        let json = serde_json::to_string(&descriptor).expect("descriptor serializes");
        let restored: Localizable = serde_json::from_str(&json).expect("descriptor deserializes");
        assert_eq!(restored, descriptor);
    }
}
