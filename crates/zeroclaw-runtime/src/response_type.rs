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
    Number,
    Choice { options: Vec<ChoiceOption> },
}

impl ResponseType {
    #[must_use]
    pub fn prompt_sigil(&self) -> PromptSigil {
        match self {
            ResponseType::Secret => PromptSigil::Secret,
            ResponseType::YesNo
            | ResponseType::FreeformText
            | ResponseType::Number
            | ResponseType::Choice { .. } => PromptSigil::Visible,
        }
    }

    #[must_use]
    pub fn expectation(&self) -> ResponseExpectation {
        match self {
            ResponseType::YesNo => ResponseExpectation::Confirmation,
            ResponseType::Secret => ResponseExpectation::HiddenText,
            ResponseType::FreeformText => ResponseExpectation::Text,
            ResponseType::Number => ResponseExpectation::Number,
            ResponseType::Choice { .. } => ResponseExpectation::Selection,
        }
    }

    #[must_use]
    pub fn ask_kind(&self) -> AskKind {
        match self {
            ResponseType::YesNo => AskKind::YesNo,
            ResponseType::Secret => AskKind::Secret,
            ResponseType::FreeformText => AskKind::FreeformText,
            ResponseType::Number => AskKind::Number,
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
    Number,
    Choice,
}

impl std::fmt::Display for AskKind {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let label = match self {
            AskKind::YesNo => "YesNo",
            AskKind::Secret => "Secret",
            AskKind::FreeformText => "FreeformText",
            AskKind::Number => "Number",
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
    Number,
    Selection,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum ResponseValue {
    YesNo(bool),
    Secret(SecretValue),
    FreeformText(String),
    Number(String),
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
                | (ResponseValue::Number(_), ResponseType::Number)
                | (ResponseValue::Choice(_), ResponseType::Choice { .. })
        )
    }

    #[must_use]
    pub fn parse_number(raw: &str) -> Option<Self> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return None;
        }
        if trimmed.parse::<i128>().is_ok() || trimmed.parse::<f64>().is_ok() {
            Some(ResponseValue::Number(trimmed.to_string()))
        } else {
            None
        }
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
            ResponseType::Choice {
                options: Vec::new()
            }
            .prompt_sigil()
            .as_str(),
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
        assert_eq!(
            ResponseType::YesNo.expectation(),
            ResponseExpectation::Confirmation
        );
        assert_eq!(
            ResponseType::Secret.expectation(),
            ResponseExpectation::HiddenText
        );
        assert_eq!(
            ResponseType::FreeformText.expectation(),
            ResponseExpectation::Text
        );
        assert_eq!(
            ResponseType::Choice {
                options: Vec::new()
            }
            .expectation(),
            ResponseExpectation::Selection
        );
    }

    #[test]
    fn ask_kind_display_renders_variant_name_verbatim() {
        assert_eq!(ResponseType::Secret.ask_kind().to_string(), "Secret");
        assert_eq!(ResponseType::YesNo.ask_kind().to_string(), "YesNo");
        assert_eq!(
            ResponseType::FreeformText.ask_kind().to_string(),
            "FreeformText"
        );
        assert_eq!(ResponseType::Number.ask_kind().to_string(), "Number");
        assert_eq!(
            ResponseType::Choice {
                options: Vec::new()
            }
            .ask_kind()
            .to_string(),
            "Choice"
        );
    }

    #[test]
    fn parse_number_accepts_integers_and_floats() {
        assert_eq!(
            ResponseValue::parse_number("42"),
            Some(ResponseValue::Number("42".into()))
        );
        assert_eq!(
            ResponseValue::parse_number("  3.14 "),
            Some(ResponseValue::Number("3.14".into()))
        );
        assert_eq!(
            ResponseValue::parse_number("18446744073709551615"),
            Some(ResponseValue::Number("18446744073709551615".into()))
        );
    }

    #[test]
    fn parse_number_rejects_non_numeric_and_empty() {
        assert_eq!(ResponseValue::parse_number("https://x"), None);
        assert_eq!(ResponseValue::parse_number(""), None);
        assert_eq!(ResponseValue::parse_number("  "), None);
    }

    #[test]
    fn number_response_matches_only_number_type() {
        let value = ResponseValue::Number("7".into());
        assert!(value.response_type_matches(&ResponseType::Number));
        assert!(!value.response_type_matches(&ResponseType::FreeformText));
    }

    #[test]
    fn number_uses_angle_sigil() {
        assert_eq!(ResponseType::Number.prompt_sigil().as_str(), ">");
    }

    #[test]
    fn number_declares_number_expectation() {
        assert_eq!(
            ResponseType::Number.expectation(),
            ResponseExpectation::Number
        );
    }
}
