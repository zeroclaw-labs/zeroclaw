use anyhow::{Result, bail};
use std::io::{BufRead, Write};

#[derive(Debug, Clone, Default)]
pub struct Input {
    prompt: String,
    default: Option<String>,
    allow_empty: bool,
}

impl Input {
    #[must_use]
    pub fn new() -> Self {
        Self {
            prompt: String::new(),
            default: None,
            allow_empty: false,
        }
    }

    #[must_use]
    pub fn with_prompt<S: Into<String>>(mut self, prompt: S) -> Self {
        self.prompt = prompt.into();
        self
    }

    #[must_use]
    pub fn allow_empty(mut self, val: bool) -> Self {
        self.allow_empty = val;
        self
    }

    #[must_use]
    pub fn default<S: Into<String>>(mut self, value: S) -> Self {
        self.default = Some(value.into());
        self
    }

    pub fn interact_text(self) -> Result<String> {
        let stdin = std::io::stdin();
        let stdout = std::io::stdout();
        self.interact_text_with_io(stdin.lock(), stdout.lock())
    }

    fn interact_text_with_io<R: BufRead, W: Write>(
        self,
        mut reader: R,
        mut writer: W,
    ) -> Result<String> {
        loop {
            write!(writer, "{}", self.render_prompt())?;
            writer.flush()?;

            let mut line = String::new();
            let bytes_read = reader.read_line(&mut line)?;
            if bytes_read == 0 {
                bail!("No input received from stdin");
            }

            let trimmed = trim_trailing_line_ending(&line);
            if trimmed.is_empty() {
                if let Some(default) = &self.default {
                    return Ok(default.clone());
                }
                if self.allow_empty {
                    return Ok(String::new());
                }
                writeln!(writer, "Input cannot be empty.")?;
                continue;
            }

            return Ok(trimmed.to_string());
        }
    }

    fn render_prompt(&self) -> String {
        match &self.default {
            Some(default) => format!("{} [{}]: ", self.prompt, default),
            None => format!("{}: ", self.prompt),
        }
    }
}

fn trim_trailing_line_ending(input: &str) -> &str {
    input.trim_end_matches(['\n', '\r'])
}

#[cfg(test)]
mod tests {
    use super::{Input, trim_trailing_line_ending};
    use anyhow::Result;
    use std::io::Cursor;

    #[test]
    fn trim_trailing_line_ending_strips_newlines() {
        assert_eq!(trim_trailing_line_ending("value\n"), "value");
        assert_eq!(trim_trailing_line_ending("value\r\n"), "value");
        assert_eq!(trim_trailing_line_ending("value\r"), "value");
        assert_eq!(trim_trailing_line_ending("value"), "value");
    }

    #[test]
    fn interact_text_returns_typed_value_without_newline() -> Result<()> {
        let input = Input::new().with_prompt("Prompt");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"typed-value\n"), &mut output)?;

        assert_eq!(value, "typed-value");
        assert_eq!(String::from_utf8(output)?, "Prompt: ");
        Ok(())
    }

    #[test]
    fn interact_text_returns_default_for_blank_input() -> Result<()> {
        let input = Input::new().with_prompt("Prompt").default("fallback");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\n"), &mut output)?;

        assert_eq!(value, "fallback");
        assert_eq!(String::from_utf8(output)?, "Prompt [fallback]: ");
        Ok(())
    }

    #[test]
    fn interact_text_allows_empty_when_requested() -> Result<()> {
        let input = Input::new().with_prompt("Prompt").allow_empty(true);
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\n"), &mut output)?;

        assert_eq!(value, "");
        assert_eq!(String::from_utf8(output)?, "Prompt: ");
        Ok(())
    }

    #[test]
    fn interact_text_reprompts_when_empty_is_not_allowed() -> Result<()> {
        let input = Input::new().with_prompt("Prompt");
        let mut output = Vec::new();

        let value = input.interact_text_with_io(Cursor::new(b"\nsecond-try\n"), &mut output)?;

        assert_eq!(value, "second-try");
        assert_eq!(
            String::from_utf8(output)?,
            "Prompt: Input cannot be empty.\nPrompt: "
        );
        Ok(())
    }
}
