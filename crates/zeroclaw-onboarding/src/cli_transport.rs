use async_trait::async_trait;
use std::io::{BufRead, Write};
use zeroclaw_runtime::flow::{FlowTransport, Outcome, Prompt, TransportError, TransportResult};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue, SecretValue};

/// The operator wants this secret left unset for now. Deliberately a typed
/// word, not bare Enter: an accidental Enter on a masked prompt must re-ask,
/// not silently skip a credential.
pub fn is_secret_deferral(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.eq_ignore_ascii_case("later")
        || trimmed.eq_ignore_ascii_case("skip")
        || trimmed.eq_ignore_ascii_case("not yet")
}

#[async_trait]
pub trait CliSecretSource: Send {
    async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String>;
}

/// Interactive multi-line authoring seam. A freeform prompt carrying an
/// `editor_seed` routes here so an operator can edit a pre-filled buffer in
/// `$EDITOR` instead of typing a single line. The default spawns the system
/// editor on a temp file; tests inject a scripted session.
pub trait CliEditorSession: Send {
    fn edit(&mut self, seed: &str) -> TransportResult<String>;
}

pub struct SystemEditorSession;

impl CliEditorSession for SystemEditorSession {
    fn edit(&mut self, seed: &str) -> TransportResult<String> {
        let mut file = tempfile::Builder::new()
            .suffix(".md")
            .tempfile()
            .map_err(|_| TransportError::Closed)?;
        use std::io::Write as _;
        file.write_all(seed.as_bytes())
            .map_err(|_| TransportError::Closed)?;
        file.flush().map_err(|_| TransportError::Closed)?;
        zeroclaw_runtime::editor::open_in_editor(file.path())
            .map_err(|_| TransportError::Closed)?;
        std::fs::read_to_string(file.path()).map_err(|_| TransportError::Closed)
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
    editor: Box<dyn CliEditorSession>,
    editor_enabled: bool,
}

impl<R: BufRead + Send, W: Write + Send, S: CliSecretSource> CliTransport<R, W, S> {
    pub fn with_secret_source(reader: R, writer: W, secret_source: S) -> Self {
        Self {
            reader,
            writer,
            secret_source,
            editor: Box::new(SystemEditorSession),
            editor_enabled: false,
        }
    }

    /// Enable interactive `$EDITOR` authoring for freeform prompts that carry an
    /// editor seed. Off by default so scripted/non-tty runs read a line; the CLI
    /// path turns it on only when stdin is an interactive terminal.
    #[must_use]
    pub fn with_interactive_editor(mut self, enabled: bool) -> Self {
        self.editor_enabled = enabled;
        self
    }

    #[must_use]
    pub fn with_editor(mut self, editor: Box<dyn CliEditorSession>) -> Self {
        self.editor = editor;
        self.editor_enabled = true;
        self
    }

    fn prompt_line(&self, prompt: &Prompt) -> String {
        let hint = if prompt.routes_secret() {
            " (type 'later' to leave it unset for now)"
        } else {
            ""
        };
        format!(
            "{}{} {}\n",
            crate::i18n::resolve_prompt_text(prompt),
            hint,
            prompt.sigil().as_str()
        )
    }

    fn editor_target(&self, prompt: &Prompt) -> Option<String> {
        if !self.editor_enabled {
            return None;
        }
        match prompt.response_type {
            ResponseType::FreeformText => prompt.editor_seed.clone(),
            _ => None,
        }
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
                } else if is_secret_deferral(raw) {
                    // 'later' typed into the masked prompt: leave the field
                    // unset instead of holding the walk hostage until the
                    // person produces a token they may not have yet.
                    Some(ResponseValue::Secret(SecretValue::new(String::new())))
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
                let secret_prompt = crate::i18n::resolve_prompt_text(prompt);
                self.secret_source.read_secret(&secret_prompt).await?
            } else if let Some(seed) = self.editor_target(prompt) {
                self.editor.edit(&seed)?
            } else {
                self.read_line()?
            };
            if let Some(value) = Self::parse(prompt, &raw) {
                return Ok(value);
            }
        }
    }

    async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
        let descriptor = crate::outcome_message::outcome_message(outcome);
        let args: Vec<(&str, &str)> = descriptor
            .args
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let localized =
            crate::i18n::get_required_onboard_string_with_args(&descriptor.message_id, &args);
        let rendered = format!("{localized}\n[{}]\n", outcome.label());
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
        assert_eq!(
            rendered,
            "Token (type 'later' to leave it unset for now) #\n"
        );
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

    struct ScriptedEditor {
        seen_seed: std::sync::Arc<std::sync::Mutex<Option<String>>>,
        returns: String,
    }

    impl CliEditorSession for ScriptedEditor {
        fn edit(&mut self, seed: &str) -> TransportResult<String> {
            *self.seen_seed.lock().unwrap() = Some(seed.to_string());
            Ok(self.returns.clone())
        }
    }

    #[tokio::test]
    async fn freeform_with_seed_routes_through_editor_when_enabled() {
        let mut output: Vec<u8> = Vec::new();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let editor = ScriptedEditor {
            seen_seed: seen.clone(),
            returns: "edited body".to_string(),
        };
        let mut transport = CliTransport::with_secret_source(
            Cursor::new(Vec::new()),
            &mut output,
            ScriptedSecretSource::new(Vec::new()),
        )
        .with_editor(Box::new(editor));
        let prompt = Prompt::new("Write SOUL.md", ResponseType::FreeformText)
            .with_editor_seed("template seed");
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::FreeformText("edited body".into()));
        assert_eq!(seen.lock().unwrap().as_deref(), Some("template seed"));
    }

    #[tokio::test]
    async fn freeform_with_seed_reads_line_when_editor_disabled() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(b"typed inline\n".to_vec()), &mut output);
        let prompt = Prompt::new("Write SOUL.md", ResponseType::FreeformText)
            .with_editor_seed("template seed");
        let value = transport.ask(&prompt).await.unwrap();
        assert_eq!(value, ResponseValue::FreeformText("typed inline".into()));
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
    async fn secret_deferral_yields_empty_secret() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = CliTransport::with_secret_source(
            Cursor::new(Vec::new()),
            &mut output,
            ScriptedSecretSource::new(vec!["later"]),
        );
        let prompt = Prompt::new("Token", ResponseType::Secret);
        let value = transport.ask(&prompt).await.unwrap();
        let ResponseValue::Secret(secret) = value else {
            panic!("secret prompt yields a secret value");
        };
        assert!(secret.expose().is_empty(), "'later' defers the credential");
        let rendered = String::from_utf8(output).unwrap();
        assert!(
            rendered.contains("type 'later'"),
            "prompt advertises the deferral escape: {rendered}"
        );
    }

    #[test]
    fn secret_deferral_matrix() {
        for defer in ["later", "LATER", "skip", "not yet", " later "] {
            assert!(is_secret_deferral(defer), "should defer: {defer}");
        }
        for keep in ["", "sk-real-token", "laterx", "skipperoo"] {
            assert!(!is_secret_deferral(keep), "should NOT defer: {keep}");
        }
    }

    #[tokio::test]
    async fn emit_renders_localized_message_and_structural_token() {
        let mut output: Vec<u8> = Vec::new();
        let mut transport = visible_only(Cursor::new(Vec::new()), &mut output);
        let outcome = Outcome::Completed {
            configured: vec![ConfiguredItem {
                layer: "channel".into(),
                instance: "matrix".into(),
            }],
        };
        transport.emit(&outcome).await.unwrap();
        let rendered = String::from_utf8(output).unwrap();
        assert!(
            rendered.contains("[completed: channel:matrix]"),
            "structural token must survive for transcript matching, got:\n{rendered}"
        );
        assert!(
            rendered.contains("channel:matrix"),
            "localized message must carry the items arg, got:\n{rendered}"
        );
    }
}
