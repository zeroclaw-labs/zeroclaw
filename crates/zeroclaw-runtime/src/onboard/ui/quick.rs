//! Headless `OnboardUi` backend for `--quick` (scripted / CI) runs.
//!
//! Prompt text is the lookup key into `answers`. Unanswered prompts fall back
//! to the caller-supplied `current`/`default`; when neither is available the
//! call errors so a malformed script fails loudly instead of hanging or
//! silently picking a wrong option.

use std::collections::HashMap;

use anyhow::{Result, bail};
use async_trait::async_trait;
use zeroclaw_config::traits::{OnboardUi, SelectItem};

#[derive(Debug, Default)]
pub struct QuickUi {
    answers: HashMap<String, String>,
}

impl QuickUi {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, prompt: impl Into<String>, value: impl Into<String>) -> Self {
        self.answers.insert(prompt.into(), value.into());
        self
    }
}

#[async_trait]
impl OnboardUi for QuickUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        match self.answers.get(prompt) {
            Some(value) => Ok(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "true" | "yes" | "y" | "1"
            )),
            None => Ok(default),
        }
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<String> {
        if let Some(answer) = self.answers.get(prompt) {
            return Ok(answer.clone());
        }
        if let Some(value) = current {
            return Ok(value.to_string());
        }
        bail!("quick mode: no answer or default provided for prompt {prompt:?}");
    }

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Option<String>> {
        match (self.answers.get(prompt), has_current) {
            (Some(value), _) => Ok(Some(value.clone())),
            (None, true) => Ok(None),
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
    ) -> Result<usize> {
        if let Some(answer) = self.answers.get(prompt) {
            if let Some(index) = items
                .iter()
                .position(|item| item.label.eq_ignore_ascii_case(answer))
            {
                return Ok(index);
            }
            bail!("quick mode: {prompt:?} answer {answer:?} matches none of the available options");
        }
        if let Some(index) = current {
            return Ok(index);
        }
        bail!("quick mode: no answer or default provided for prompt {prompt:?}");
    }

    async fn editor(&mut self, _hint: &str, initial: &str) -> Result<String> {
        Ok(initial.to_string())
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
