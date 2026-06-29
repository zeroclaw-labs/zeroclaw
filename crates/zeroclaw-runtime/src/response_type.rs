use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct ChoiceOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ResponseType {
    YesNo,
    Secret,
    FreeformText,
    Choice { options: Vec<ChoiceOption> },
}

impl ResponseType {
    #[must_use]
    pub fn prompt_sigil(&self) -> PromptSigil {
        match self {
            ResponseType::Secret => PromptSigil::Secret,
            ResponseType::YesNo | ResponseType::FreeformText | ResponseType::Choice { .. } => {
                PromptSigil::Visible
            }
        }
    }

    #[must_use]
    pub fn expectation(&self) -> ResponseExpectation {
        match self {
            ResponseType::YesNo => ResponseExpectation::Confirmation,
            ResponseType::Secret => ResponseExpectation::HiddenText,
            ResponseType::FreeformText => ResponseExpectation::Text,
            ResponseType::Choice { .. } => ResponseExpectation::Selection,
        }
    }

    #[must_use]
    pub fn ask_kind(&self) -> AskKind {
        match self {
            ResponseType::YesNo => AskKind::YesNo,
            ResponseType::Secret => AskKind::Secret,
            ResponseType::FreeformText => AskKind::FreeformText,
            ResponseType::Choice { .. } => AskKind::Choice,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    YesNo,
    Secret,
    FreeformText,
    Choice,
}

impl std::fmt::Display for AskKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            AskKind::YesNo => "YesNo",
            AskKind::Secret => "Secret",
            AskKind::FreeformText => "FreeformText",
            AskKind::Choice => "Choice",
        };
        formatter.write_str(label)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptSigil {
    Visible,
    Secret,
}

impl PromptSigil {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            PromptSigil::Visible => ">",
            PromptSigil::Secret => "#",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseExpectation {
    Confirmation,
    HiddenText,
    Text,
    Selection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ResponseValue {
    YesNo(bool),
    Secret(SecretValue),
    FreeformText(String),
    Choice(String),
}

impl ResponseValue {
    #[must_use]
    pub fn response_type_matches(&self, response_type: &ResponseType) -> bool {
        matches!(
            (self, response_type),
            (ResponseValue::YesNo(_), ResponseType::YesNo)
                | (ResponseValue::Secret(_), ResponseType::Secret)
                | (ResponseValue::FreeformText(_), ResponseType::FreeformText)
                | (ResponseValue::Choice(_), ResponseType::Choice { .. })
        )
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SecretValue(String);

impl SecretValue {
    #[must_use]
    pub fn new(raw: String) -> Self {
        Self(raw)
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl std::fmt::Debug for SecretValue {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SecretValue(<redacted>)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FollowOn {
    BackToLlm,
    ToolCall { tool: String },
    NextNode { node: String },
    Complete,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_response_type_uses_hash_sigil() {
        assert_eq!(ResponseType::Secret.prompt_sigil().as_str(), "#");
    }

    #[test]
    fn visible_response_types_use_angle_sigil() {
        assert_eq!(ResponseType::YesNo.prompt_sigil().as_str(), ">");
        assert_eq!(ResponseType::FreeformText.prompt_sigil().as_str(), ">");
        assert_eq!(
            ResponseType::Choice { options: Vec::new() }.prompt_sigil().as_str(),
            ">"
        );
    }

    #[test]
    fn secret_value_debug_is_redacted() {
        let secret = SecretValue::new("super-secret-token".into());
        let rendered = format!("{secret:?}");
        assert!(!rendered.contains("super-secret-token"));
        assert!(rendered.contains("redacted"));
    }

    #[test]
    fn response_value_matches_its_type() {
        assert!(ResponseValue::YesNo(true).response_type_matches(&ResponseType::YesNo));
        assert!(
            ResponseValue::Secret(SecretValue::new("x".into()))
                .response_type_matches(&ResponseType::Secret)
        );
        assert!(
            !ResponseValue::FreeformText("x".into()).response_type_matches(&ResponseType::YesNo)
        );
    }

    #[test]
    fn each_response_type_declares_its_expectation() {
        assert_eq!(ResponseType::YesNo.expectation(), ResponseExpectation::Confirmation);
        assert_eq!(ResponseType::Secret.expectation(), ResponseExpectation::HiddenText);
        assert_eq!(ResponseType::FreeformText.expectation(), ResponseExpectation::Text);
        assert_eq!(
            ResponseType::Choice { options: Vec::new() }.expectation(),
            ResponseExpectation::Selection
        );
    }

    #[test]
    fn ask_kind_display_renders_variant_name_verbatim() {
        assert_eq!(ResponseType::Secret.ask_kind().to_string(), "Secret");
        assert_eq!(ResponseType::YesNo.ask_kind().to_string(), "YesNo");
        assert_eq!(ResponseType::FreeformText.ask_kind().to_string(), "FreeformText");
        assert_eq!(
            ResponseType::Choice { options: Vec::new() }.ask_kind().to_string(),
            "Choice"
        );
    }
}
