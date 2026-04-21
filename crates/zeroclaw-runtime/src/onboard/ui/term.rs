//! Terminal `OnboardUi` backend built on `dialoguer`.
//!
//! Dialoguer is blocking, so every async trait method wraps its call in
//! `tokio::task::spawn_blocking`. The struct carries no state — it's a marker.

use anyhow::Result;
use async_trait::async_trait;
use dialoguer::{Confirm, Editor, Input, Password, Select};
use zeroclaw_config::traits::{OnboardUi, SelectItem};

pub struct TermUi;

#[async_trait]
impl OnboardUi for TermUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<bool> {
        let prompt = prompt.to_string();
        tokio::task::spawn_blocking(move || {
            Confirm::new()
                .with_prompt(prompt)
                .default(default)
                .interact()
                .map_err(anyhow::Error::from)
        })
        .await?
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<String> {
        let prompt = prompt.to_string();
        let default = current.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || {
            let mut input = Input::<String>::new().with_prompt(prompt);
            if let Some(value) = default {
                input = input.default(value);
            }
            input.interact_text().map_err(anyhow::Error::from)
        })
        .await?
    }

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Option<String>> {
        let prompt = prompt.to_string();
        tokio::task::spawn_blocking(move || -> Result<Option<String>> {
            if has_current {
                let replace = Confirm::new()
                    .with_prompt(format!("{prompt} (stored, replace?)"))
                    .default(false)
                    .interact()?;
                if !replace {
                    return Ok(None);
                }
            }
            let value = Password::new().with_prompt(prompt).interact()?;
            Ok(Some(value))
        })
        .await?
    }

    async fn select(
        &mut self,
        prompt: &str,
        items: &[SelectItem],
        current: Option<usize>,
    ) -> Result<usize> {
        let prompt = prompt.to_string();
        let labels: Vec<String> = items
            .iter()
            .map(|item| match &item.badge {
                Some(badge) => format!("{}  {badge}", item.label),
                None => item.label.clone(),
            })
            .collect();
        tokio::task::spawn_blocking(move || {
            let mut select = Select::new().with_prompt(prompt).items(&labels);
            if let Some(index) = current {
                select = select.default(index);
            }
            select.interact().map_err(anyhow::Error::from)
        })
        .await?
    }

    async fn editor(&mut self, hint: &str, initial: &str) -> Result<String> {
        let hint = hint.to_string();
        let buffer = initial.to_string();
        let fallback = initial.to_string();
        tokio::task::spawn_blocking(move || -> Result<String> {
            if !hint.is_empty() {
                println!("  {hint}");
            }
            Ok(Editor::new().edit(&buffer)?.unwrap_or(fallback))
        })
        .await?
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
