use async_trait::async_trait;
use std::io::{BufRead, Write};
use zeroclaw_runtime::flow::{FlowTransport, Outcome, Prompt, TransportError, TransportResult};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue, SecretValue};

#[async_trait]
pub trait CliSecretSource: Send {
    async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String>;
}

pub struct NoSecretSource;

#[async_trait]
impl CliSecretSource for NoSecretSource {
    async fn read_secret(&mut self, _prompt_text: &str) -> TransportResult<String> {
        Err(TransportError::Closed)
    }
}

pub struct TtyPasswordSource;

#[async_trait]
impl CliSecretSource for TtyPasswordSource {
    async fn read_secret(&mut self, _prompt_text: &str) -> TransportResult<String> {
        tokio::task::spawn_blocking(move || {
            dialoguer::Password::new()
                .with_prompt("")
                .report(false)
                .interact_on(&dialoguer::console::Term::stdout())
                .map_err(|_| TransportError::Closed)
        })
        .await
        .map_err(|_| TransportError::Closed)?
    }
}

pub struct CliTransport<R: BufRead + Send, W: Write + Send, S: CliSecretSource> {
    reader: R,
    writer: W,
    secret_source: S,
}

impl<R: BufRead + Send, W: Write + Send, S: CliSecretSource> CliTransport<R, W, S> {
    pub fn with_secret_source(reader: R, writer: W, secret_source: S) -> Self {
        Self {
            reader,
            writer,
            secret_source,
        }
    }

    fn prompt_line(&self, prompt: &Prompt) -> String {
        format!("{} {}\n", prompt.text, prompt.sigil().as_str())
    }

    fn read_line(&mut self) -> TransportResult<String> {
        let mut line = String::new();
        let read = self
            .reader
            .read_line(&mut line)
            .map_err(|_| TransportError::Closed)?;
        if read == 0 {
            return Err(TransportError::Closed);
        }
        Ok(line.trim_end_matches(['\n', '\r']).to_string())
    }

    fn parse(prompt: &Prompt, raw: &str) -> Option<ResponseValue> {
        match &prompt.response_type {
            ResponseType::Secret => {
                if raw.is_empty() {
                    None
                } else {
                    Some(ResponseValue::Secret(SecretValue::new(raw.to_string())))
                }
            }
            ResponseType::FreeformText => {
                if raw.is_empty() {
                    None
                } else {
                    Some(ResponseValue::FreeformText(raw.to_string()))
                }
            }
            ResponseType::Number => ResponseValue::parse_number(raw),
            ResponseType::YesNo => parse_yes_no(raw).map(ResponseValue::YesNo),
            ResponseType::Choice { options } => options
                .iter()
                .find(|option| option.value == raw)
                .map(|option| ResponseValue::Choice(option.value.clone())),
        }
    }
}

fn parse_yes_no(raw: &str) -> Option<bool> {
    let normalized = raw.to_ascii_lowercase();
    let affirmative = ["y", "yes"];
    let negative = ["n", "no"];
    if affirmative.contains(&normalized.as_str()) {
        Some(true)
    } else if negative.contains(&normalized.as_str()) {
        Some(false)
    } else {
        None
    }
}

#[async_trait]
impl<R: BufRead + Send, W: Write + Send, S: CliSecretSource> FlowTransport
    for CliTransport<R, W, S>
{
    async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
        loop {
            let line = self.prompt_line(prompt);
            self.writer
                .write_all(line.as_bytes())
                .map_err(|_| TransportError::Closed)?;
            self.writer.flush().map_err(|_| TransportError::Closed)?;
            let raw = if prompt.routes_secret() {
                self.secret_source.read_secret(&prompt.text).await?
            } else {
                self.read_line()?
            };
            if let Some(value) = Self::parse(prompt, &raw) {
                return Ok(value);
            }
        }
    }

    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
        let rendered = format!("[{}]\n", outcome.label());
        self.writer
            .write_all(rendered.as_bytes())
            .map_err(|_| TransportError::Closed)?;
        self.writer.flush().map_err(|_| TransportError::Closed)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::io::Cursor;
    use zeroclaw_runtime::flow::ConfiguredItem;
    use zeroclaw_runtime::response_type::ChoiceOption;

    struct ScriptedSecretSource {
        secrets: VecDeque<String>,
        prompts_seen: Vec<String>,
    }

    impl ScriptedSecretSource {
        fn new(secrets: Vec<&str>) -> Self {
            Self {
                secrets: secrets.into_iter().map(String::from).collect(),
                prompts_seen: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl CliSecretSource for ScriptedSecretSource {
        async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String> {
            self.prompts_seen.push(prompt_text.to_string());
            self.secrets.pop_front().ok_or(TransportError::Closed)
        }
    }

    fn visible_only(
        reader: Cursor<Vec<u8>>,
        writer: &mut Vec<u8>,
    ) -> CliTransport<Cursor<Vec<u8>>, &mut Vec<u8>, ScriptedSecretSource> {
        CliTransport::with_secret_source(reader, writer, ScriptedSecretSource::new(Vec::new()))
    }

    #[tokio::test]
    async fn yes_no_prompt_uses_angle_sigil_and_parses_affirmative() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(b"yes\n".to_vec()), &mut output);
        let prompt = Prompt::new("Proceed?", ResponseType::YesNo);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::YesNo(true));
        assert_eq!(String::from_utf8(output).unwrap(), "Proceed? >\n");
    }

    #[tokio::test]
    async fn secret_prompt_writes_its_sigil_line_but_never_the_value() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = CliTransport::with_secret_source(
            Cursor::new(Vec::new()),
            &mut output,
            ScriptedSecretSource::new(vec!["sk-secret"]),
        );
        let prompt = Prompt::new("Token", ResponseType::Secret);
        let value = transport.ask(&prompt).await.unwrap();
        match value {
            ResponseValue::Secret(secret) => assert_eq!(secret.expose(), "sk-secret"),
            other => panic!("expected secret, got {other:?}"),
        }
        let rendered = String::from_utf8(output).unwrap();
        assert_eq!(rendered, "Token #\n");
        assert!(!rendered.contains("sk-secret"));
    }

    #[tokio::test]
    async fn invalid_yes_no_reprompts() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(b"maybe\nno\n".to_vec()), &mut output);
        let prompt = Prompt::new("Proceed?", ResponseType::YesNo);
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::YesNo(false));
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "Proceed? >\nProceed? >\n"
        );
    }

    #[tokio::test]
    async fn choice_parses_matching_option_value() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(b"partial\n".to_vec()), &mut output);
        let prompt = Prompt::new(
            "Mode",
            ResponseType::Choice {
                options: vec![
                    ChoiceOption {
                        value: "full".into(),
                        label: "Full".into(),
                    },
                    ChoiceOption {
                        value: "partial".into(),
                        label: "Partial".into(),
                    },
                ],
            },
        );
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::Choice("partial".into()));
    }

    #[tokio::test]
    async fn closed_secret_source_errors() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(Vec::new()), &mut output);
        let prompt = Prompt::new("Token", ResponseType::Secret);
        let result = transport.ask(&prompt).await;
        assert!(matches!(result, Err(TransportError::Closed)));
    }

    #[tokio::test]
    async fn emit_renders_outcome_label() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(Vec::new()), &mut output);
        let outcome = Outcome::Completed {
            configured: vec![ConfiguredItem {
                layer: "channel".into(),
                instance: "matrix".into(),
            }],
        };
        transport.emit(&outcome).await.unwrap();
        assert_eq!(
            String::from_utf8(output).unwrap(),
            "[completed: channel:matrix]\n"
        );
    }
}
