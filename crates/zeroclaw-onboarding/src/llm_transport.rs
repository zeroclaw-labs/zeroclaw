use async_trait::async_trait;
use zeroclaw_runtime::flow::{FlowTransport, Outcome, Prompt, TransportResult};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue, SecretValue};

#[async_trait]
pub trait LlmResponder: Send {
    async fn respond(&mut self, prompt_text: &str) -> TransportResult<String>;
}

#[async_trait]
pub trait SecretReader: Send {
    async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String>;
}

pub struct TtySecretReader;

#[async_trait]
impl SecretReader for TtySecretReader {
    async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String> {
        let prompt = prompt_text.to_string();
        tokio::task::spawn_blocking(move || {
            dialoguer::Password::new()
                .with_prompt(prompt)
                .interact_on(&dialoguer::console::Term::stdout())
                .map_err(|_| zeroclaw_runtime::flow::TransportError::Closed)
        })
        .await
        .map_err(|_| zeroclaw_runtime::flow::TransportError::Closed)?
    }
}

pub struct LlmTransport<L: LlmResponder, S: SecretReader> {
    responder: L,
    secret_reader: S,
    emitted: Vec<Outcome>,
}

impl<L: LlmResponder, S: SecretReader> LlmTransport<L, S> {
    pub fn new(responder: L, secret_reader: S) -> Self {
        Self {
            responder,
            secret_reader,
            emitted: Vec::new(),
        }
    }

    pub fn emitted(&self) -> &[Outcome] {
        &self.emitted
    }

    pub fn into_responder(self) -> L {
        self.responder
    }

    fn parse_non_secret(prompt: &Prompt, raw: &str) -> Option<ResponseValue> {
        parse_raw(prompt, raw)
    }
}

/// Parse a raw string against a prompt's response type. The single value
/// parser for every LLM-fed transport, so freeform submissions and per-field
/// answers accept exactly the same shapes.
pub(crate) fn parse_raw(prompt: &Prompt, raw: &str) -> Option<ResponseValue> {
    let trimmed = raw.trim();
    match &prompt.response_type {
        ResponseType::Secret => None,
        ResponseType::FreeformText => {
            if trimmed.is_empty() {
                None
            } else {
                Some(ResponseValue::FreeformText(trimmed.to_string()))
            }
        }
        ResponseType::Number => ResponseValue::parse_number(trimmed),
        ResponseType::YesNo => parse_yes_no(trimmed).map(ResponseValue::YesNo),
        ResponseType::Choice { options } => options
            .iter()
            .find(|option| option.value == trimmed)
            .map(|option| ResponseValue::Choice(option.value.clone())),
    }
}

pub(crate) fn parse_yes_no(raw: &str) -> Option<bool> {
    let normalized = raw.to_ascii_lowercase();
    let affirmative = ["y", "yes", "true"];
    let negative = ["n", "no", "false"];
    if affirmative.contains(&normalized.as_str()) {
        Some(true)
    } else if negative.contains(&normalized.as_str()) {
        Some(false)
    } else {
        None
    }
}

/// The agent's way of declining to set an optional field. Returned for an
/// optional prompt only; the walk's `response_is_empty` skip-path then leaves
/// the property unwritten. Without this an optional `Option<_>` or optional
/// list field re-prompts forever, because no concrete value means "no value".
fn is_skip_sentinel(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("skip")
}

/// An empty response carrier matching the prompt's type, so the walk recognizes
/// it as "no value" via `response_is_empty`.
fn empty_response_for(prompt: &Prompt) -> ResponseValue {
    match &prompt.response_type {
        ResponseType::Number => ResponseValue::Number(String::new()),
        ResponseType::Choice { .. } => ResponseValue::Choice(String::new()),
        ResponseType::Secret => ResponseValue::Secret(SecretValue::new(String::new())),
        ResponseType::FreeformText | ResponseType::YesNo => {
            ResponseValue::FreeformText(String::new())
        }
    }
}

/// Upper bound on agent turns for a single prompt. A non-progressing walk
/// (model never produces a parseable value) fails fast instead of hanging.
const MAX_REASK_PER_PROMPT: usize = 6;

/// The machine contract for the `ANSWER:` value, appended to every prompt the
/// guide receives. Without it the guide cannot know a choice field's valid
/// values and answers with a paraphrase (`on`, `none`), which parses to
/// nothing; the walk then re-asks the identical text until the turn budget
/// aborts the whole flow.
fn answer_contract(prompt: &Prompt) -> String {
    let mut contract = match &prompt.response_type {
        ResponseType::Choice { options } => {
            let mut text = String::from(
                "\nThe ANSWER line must be exactly one of these values (the person sees your words, not these tokens):",
            );
            for option in options {
                text.push_str(&format!("\n- `{}` = {}", option.value, option.label));
            }
            text
        }
        ResponseType::YesNo => "\nThe ANSWER line must be exactly `yes` or `no`.".to_string(),
        ResponseType::Number => "\nThe ANSWER line must be a bare number.".to_string(),
        ResponseType::FreeformText | ResponseType::Secret => String::new(),
    };
    if prompt.optional {
        contract.push_str("\nThis field is optional: `ANSWER: none` leaves it unset.");
    }
    contract
}

#[async_trait]
impl<L: LlmResponder, S: SecretReader> FlowTransport for LlmTransport<L, S> {
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
        let prompt_text = crate::i18n::resolve_prompt_text(prompt);
        if prompt.routes_secret() {
            loop {
                let raw = self.secret_reader.read_secret(&prompt_text).await?;
                if crate::cli_transport::is_secret_deferral(&raw) {
                    // Typed deferral: leave the credential unset and move on.
                    return Ok(ResponseValue::Secret(SecretValue::new(String::new())));
                }
                if !raw.is_empty() {
                    return Ok(ResponseValue::Secret(SecretValue::new(raw)));
                }
            }
        }
        let contract = answer_contract(prompt);
        let mut message = format!("{prompt_text}{contract}");
        for _ in 0..MAX_REASK_PER_PROMPT {
            let raw = self.responder.respond(&message).await?;
            if prompt.optional && is_skip_sentinel(&raw) {
                return Ok(empty_response_for(prompt));
            }
            if let Some(value) = Self::parse_non_secret(prompt, &raw) {
                return Ok(value);
            }
            message = format!(
                "Your `ANSWER: {raw}` was not accepted for this field.{contract}\n\
                 Reply with an ANSWER line carrying exactly one accepted value."
            );
        }
        Err(zeroclaw_runtime::flow::TransportError::Agent {
            reason: format!(
                "guide produced no parseable value for prompt after {MAX_REASK_PER_PROMPT} turns"
            ),
        })
    }

    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
        self.emitted.push(outcome.clone());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use zeroclaw_runtime::flow::TransportError;
    use zeroclaw_runtime::response_type::ChoiceOption;

    struct ScriptedResponder {
        replies: VecDeque<String>,
        calls: Vec<String>,
    }

    impl ScriptedResponder {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                calls: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl LlmResponder for ScriptedResponder {
        async fn respond(&mut self, prompt_text: &str) -> TransportResult<String> {
            self.calls.push(prompt_text.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    struct RecordingSecretReader {
        replies: VecDeque<String>,
        calls: Vec<String>,
    }

    impl RecordingSecretReader {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                calls: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl SecretReader for RecordingSecretReader {
        async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String> {
            self.calls.push(prompt_text.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    struct PanicResponder;

    #[async_trait]
    impl LlmResponder for PanicResponder {
        async fn respond(&mut self, _prompt_text: &str) -> TransportResult<String> {
            panic!("the LLM responder must never be called for a secret prompt");
        }
    }

    struct PanicSecretReader;

    #[async_trait]
    impl SecretReader for PanicSecretReader {
        async fn read_secret(&mut self, _prompt_text: &str) -> TransportResult<String> {
            panic!("the secret reader must never be called for a non-secret prompt");
        }
    }

    #[tokio::test]
    async fn secret_prompt_bypasses_llm_and_reads_from_secret_channel() {
        let mut transport =
            LlmTransport::new(PanicResponder, RecordingSecretReader::new(vec!["sk-live"]));
        let prompt = Prompt::new("API key", ResponseType::Secret);
        let value = transport.ask(&prompt).await.unwrap();
        match value {
            ResponseValue::Secret(secret) => assert_eq!(secret.expose(), "sk-live"),
            other => panic!("expected secret, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn secret_deferral_yields_empty_secret_without_retry_loop() {
        let mut transport =
            LlmTransport::new(PanicResponder, RecordingSecretReader::new(vec!["later"]));
        let prompt = Prompt::new("API key", ResponseType::Secret);
        let value = transport.ask(&prompt).await.unwrap();
        match value {
            ResponseValue::Secret(secret) => {
                assert!(secret.expose().is_empty(), "'later' defers the secret");
            }
            other => panic!("expected secret, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_secret_value_never_appears_in_any_text_sent_to_the_llm() {
        const SECRET: &str = "sk-live-must-not-leak";
        let mut responder = ScriptedResponder::new(vec!["scout"]);
        let secret_reader = RecordingSecretReader::new(vec![SECRET]);
        let mut transport = LlmTransport::new(responder, secret_reader);
        transport
            .ask(&Prompt::new("API key", ResponseType::Secret))
            .await
            .unwrap();
        transport
            .ask(&Prompt::new("Agent name", ResponseType::FreeformText))
            .await
            .unwrap();
        responder = transport.into_responder();
        assert!(
            responder.calls.iter().all(|call| !call.contains(SECRET)),
            "the secret value reached the LLM responder"
        );
    }

    #[tokio::test]
    async fn non_secret_prompt_never_touches_secret_reader() {
        let mut transport =
            LlmTransport::new(ScriptedResponder::new(vec!["yes"]), PanicSecretReader);
        let prompt = Prompt::new("Proceed?", ResponseType::YesNo);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::YesNo(true));
    }

    #[tokio::test]
    async fn llm_reply_is_reparsed_until_valid() {
        let mut transport = LlmTransport::new(
            ScriptedResponder::new(vec!["maybe", "no"]),
            PanicSecretReader,
        );
        let prompt = Prompt::new("Proceed?", ResponseType::YesNo);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::YesNo(false));
    }

    #[tokio::test]
    async fn choice_matches_option_value_from_llm() {
        let mut transport =
            LlmTransport::new(ScriptedResponder::new(vec!["partial"]), PanicSecretReader);
        let prompt = Prompt::new(
            "Mode",
            ResponseType::Choice {
                options: vec![ChoiceOption {
                    value: "partial".into(),
                    label: "Partial".into(),
                }],
            },
        );
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::Choice("partial".into()));
    }

    #[tokio::test]
    async fn optional_field_skips_on_a_none_reply_instead_of_looping() {
        let mut transport =
            LlmTransport::new(ScriptedResponder::new(vec!["none"]), PanicSecretReader);
        let prompt = Prompt::new("Display name", ResponseType::FreeformText).with_optional(true);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(
            value,
            ResponseValue::FreeformText(String::new()),
            "an optional none reply must yield an empty response the walk treats as skip"
        );
    }

    #[tokio::test]
    async fn optional_choice_skip_yields_an_empty_choice() {
        let mut transport =
            LlmTransport::new(ScriptedResponder::new(vec!["skip"]), PanicSecretReader);
        let prompt = Prompt::new(
            "Mode",
            ResponseType::Choice {
                options: vec![ChoiceOption {
                    value: "partial".into(),
                    label: "Partial".into(),
                }],
            },
        )
        .with_optional(true);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::Choice(String::new()));
    }

    #[tokio::test]
    async fn a_required_field_that_never_parses_fails_fast_rather_than_hanging() {
        let mut transport = LlmTransport::new(
            ScriptedResponder::new(vec!["x", "x", "x", "x", "x", "x", "x", "x"]),
            PanicSecretReader,
        );
        let prompt = Prompt::new("Proceed?", ResponseType::YesNo);
        let error = transport.ask(&prompt).await.unwrap_err();
        assert!(
            matches!(error, TransportError::Agent { .. }),
            "a non-progressing walk must surface a bounded Agent error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn choice_prompt_carries_the_accepted_values_to_the_llm() {
        let mut responder = ScriptedResponder::new(vec!["skip"]);
        let mut transport = LlmTransport::new(responder, PanicSecretReader);
        let prompt = Prompt::new(
            "Bind this channel into a peer group?",
            ResponseType::Choice {
                options: vec![
                    ChoiceOption {
                        value: "skip".into(),
                        label: "Skip peer-group binding".into(),
                    },
                    ChoiceOption {
                        value: "new".into(),
                        label: "Create a new peer group".into(),
                    },
                ],
            },
        );
        transport.ask(&prompt).await.unwrap();
        responder = transport.into_responder();
        let sent = &responder.calls[0];
        assert!(
            sent.contains("`skip`") && sent.contains("`new`"),
            "the guide never saw the accepted choice values: {sent}"
        );
    }

    #[tokio::test]
    async fn a_rejected_reply_gets_corrective_feedback_instead_of_the_same_prompt() {
        let mut responder = ScriptedResponder::new(vec!["none", "skip"]);
        let mut transport = LlmTransport::new(responder, PanicSecretReader);
        let prompt = Prompt::new(
            "Bind this channel into a peer group?",
            ResponseType::Choice {
                options: vec![ChoiceOption {
                    value: "skip".into(),
                    label: "Skip peer-group binding".into(),
                }],
            },
        );
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::Choice("skip".into()));
        responder = transport.into_responder();
        assert!(
            responder.calls[1].contains("was not accepted"),
            "the re-ask must tell the guide its value was rejected: {}",
            responder.calls[1]
        );
        assert!(
            responder.calls[1].contains("`skip`"),
            "the re-ask must restate the accepted values: {}",
            responder.calls[1]
        );
    }
}
