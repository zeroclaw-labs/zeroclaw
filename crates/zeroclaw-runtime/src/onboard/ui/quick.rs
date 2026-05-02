//! Headless `OnboardUi` backend for `--quick` (scripted / CI) runs.
//!
//! Prompt text is the lookup key into `answers`. Unanswered prompts fall back
//! to the caller-supplied `current`/`default`; when neither is available the
//! call errors so a malformed script fails loudly instead of hanging or
//! silently picking a wrong option. `Answer::Back` is never returned — quick
//! mode has no interactive user to rewind.

use std::collections::HashMap;

use anyhow::{Result, bail};
use async_trait::async_trait;
use zeroclaw_config::traits::{Answer, OnboardUi, SecretPromptAnswer, SelectItem};

#[derive(Debug, Default)]
pub struct QuickUi {
    answers: HashMap<String, String>,
    /// Prompts that fire more than once per run (e.g. a "Channel" select
    /// hit once to enter a channel and again to pick "Done") need distinct
    /// answers per call. Sequence entries are consumed in order; if the
    /// cursor runs off the end, lookup falls back to `answers`.
    sequences: HashMap<String, Vec<String>>,
    sequence_cursor: HashMap<String, usize>,
}

impl QuickUi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, prompt: impl Into<String>, value: impl Into<String>) -> Self {
        self.answers.insert(prompt.into(), value.into());
        self
    }

    /// Register a sequence of answers for a prompt that fires multiple
    /// times. The first hit returns `values[0]`, second returns `values[1]`,
    /// etc. After the sequence is exhausted, subsequent hits fall through
    /// to `answers` / the prompt's own default.
    pub fn with_sequence<I, S>(mut self, prompt: impl Into<String>, values: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sequences
            .insert(prompt.into(), values.into_iter().map(Into::into).collect());
        self
    }

    /// Look up the next answer for `prompt`: sequence cursor first (and
    /// advance it), then the single-answer map.
    fn lookup(&mut self, prompt: &str) -> Option<String> {
        if let Some(seq) = self.sequences.get(prompt) {
            let cursor = self.sequence_cursor.entry(prompt.to_string()).or_insert(0);
            if let Some(v) = seq.get(*cursor) {
                *cursor += 1;
                return Some(v.clone());
            }
        }
        self.answers.get(prompt).cloned()
    }
}

#[async_trait]
impl OnboardUi for QuickUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<Answer<bool>> {
        Ok(Answer::Value(match self.lookup(prompt) {
            Some(value) => matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "yes" | "y" | "1"
            ),
            None => default,
        }))
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<Answer<String>> {
        if let Some(answer) = self.lookup(prompt) {
            return Ok(Answer::Value(answer));
        }
        if let Some(value) = current {
            return Ok(Answer::Value(value.to_string()));
        }
        bail!("quick mode: no answer or default provided for prompt {prompt:?}");
    }

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Answer<Option<String>>> {
        match (self.lookup(prompt), has_current) {
            (Some(value), _) => Ok(Answer::Value(Some(value))),
            (None, true) => Ok(Answer::Value(None)),
            (None, false) => {
                bail!("quick mode: secret {prompt:?} is required but no value was supplied")
            }
        }
    }

    async fn secret_with_action(
        &mut self,
        prompt: &str,
        has_current: bool,
        action_hint: &str,
    ) -> Result<SecretPromptAnswer> {
        match (self.lookup(prompt), has_current) {
            (Some(value), _) if value == "<tab>" => Ok(SecretPromptAnswer::Action),
            (Some(value), _) => Ok(SecretPromptAnswer::Value(Some(value))),
            (None, true) => Ok(SecretPromptAnswer::Value(None)),
            (None, false) if !action_hint.is_empty() => Ok(SecretPromptAnswer::Value(None)),
            (None, false) => {
                bail!("quick mode: secret {prompt:?} is required but no value was supplied")
            }
        }
    }

    async fn select(
        &mut self,
        prompt: &str,
        items: &[SelectItem],
        current: Option<usize>,
    ) -> Result<Answer<usize>> {
        if let Some(answer) = self.lookup(prompt) {
            if let Some(index) = items
                .iter()
                .position(|item| item.label.eq_ignore_ascii_case(&answer))
            {
                return Ok(Answer::Value(index));
            }
            bail!("quick mode: {prompt:?} answer {answer:?} matches none of the available options");
        }
        if let Some(index) = current {
            return Ok(Answer::Value(index));
        }
        bail!("quick mode: no answer or default provided for prompt {prompt:?}");
    }

    async fn editor(&mut self, _hint: &str, initial: &str) -> Result<Answer<String>> {
        Ok(Answer::Value(initial.to_string()))
    }

    fn heading(&mut self, level: u8, text: &str) {
        let marker = "#".repeat(level.clamp(1, 6) as usize);
        println!("\n{marker} {text}");
    }

    fn note(&mut self, msg: &str) {
        println!("\n{msg}\n");
    }

    fn status(&mut self, msg: &str) {
        println!("  {msg}");
    }

    fn warn(&mut self, msg: &str) {
        eprintln!("⚠️  {msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn secret_with_action_returns_action_for_tab_script() {
        let mut ui = QuickUi::new().with("api-key", "<tab>");

        let answer = ui
            .secret_with_action("api-key", false, "OpenAI browser login")
            .await
            .unwrap();

        assert_eq!(answer, SecretPromptAnswer::Action);
    }
}
