//! Terminal `OnboardUi` backend built on `dialoguer`.
//!
//! Dialoguer is blocking, so every async trait method wraps its call in
//! `tokio::task::spawn_blocking`. Each prompt uses dialoguer's `_opt`
//! variants where available: `Esc` makes dialoguer return `None`, which we
//! map to `Answer::Back` so the orchestrator can rewind.

use anyhow::Result;
use async_trait::async_trait;
use dialoguer::{Confirm, Editor, FuzzySelect, Input, Password};
use zeroclaw_config::traits::{Answer, OnboardUi, SelectItem};

pub struct TermUi;

#[async_trait]
impl OnboardUi for TermUi {
    async fn confirm(&mut self, prompt: &str, default: bool) -> Result<Answer<bool>> {
        let prompt = prompt.to_string();
        tokio::task::spawn_blocking(move || -> Result<Answer<bool>> {
            let v = Confirm::new()
                .with_prompt(prompt)
                .default(default)
                .interact_opt()?;
            Ok(match v {
                Some(b) => Answer::Value(b),
                None => Answer::Back,
            })
        })
        .await?
    }

    async fn string(&mut self, prompt: &str, current: Option<&str>) -> Result<Answer<String>> {
        // dialoguer 0.12 dropped `_opt` variants for Input, so Esc on a text
        // prompt is a no-op here (the ratatui backend supports Back fully).
        // The main navigation points — Confirm / FuzzySelect — still honor
        // Esc, which is where Back matters most.
        let prompt = prompt.to_string();
        let default = current.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || -> Result<Answer<String>> {
            let mut input = Input::<String>::new().with_prompt(prompt).allow_empty(true);
            if let Some(value) = default {
                input = input.default(value);
            }
            Ok(Answer::Value(input.interact_text()?))
        })
        .await?
    }

    async fn secret(&mut self, prompt: &str, has_current: bool) -> Result<Answer<Option<String>>> {
        let prompt = prompt.to_string();
        tokio::task::spawn_blocking(move || -> Result<Answer<Option<String>>> {
            if has_current {
                let replace = Confirm::new()
                    .with_prompt(format!("{prompt} (stored, replace?)"))
                    .default(false)
                    .interact_opt()?;
                match replace {
                    Some(false) => return Ok(Answer::Value(None)),
                    None => return Ok(Answer::Back),
                    Some(true) => {}
                }
            }
            let value = Password::new().with_prompt(prompt).interact()?;
            Ok(Answer::Value(Some(value)))
        })
        .await?
    }

    async fn select(
        &mut self,
        prompt: &str,
        items: &[SelectItem],
        current: Option<usize>,
    ) -> Result<Answer<usize>> {
        let prompt = prompt.to_string();
        let labels: Vec<String> = items
            .iter()
            .map(|item| match &item.badge {
                Some(badge) => format!("{}  {badge}", item.label),
                None => item.label.clone(),
            })
            .collect();
        tokio::task::spawn_blocking(move || -> Result<Answer<usize>> {
            let mut select = FuzzySelect::new().with_prompt(prompt).items(&labels);
            if let Some(index) = current {
                select = select.default(index);
            }
            Ok(match select.interact_opt()? {
                Some(i) => Answer::Value(i),
                None => Answer::Back,
            })
        })
        .await?
    }

    async fn editor(&mut self, hint: &str, initial: &str) -> Result<Answer<String>> {
        let hint = hint.to_string();
        let buffer = initial.to_string();
        tokio::task::spawn_blocking(move || -> Result<Answer<String>> {
            if !hint.is_empty() {
                println!("  {hint}");
            }
            // Editor close-without-save returns None — treat as Back so users
            // who bail out of $EDITOR can rewind instead of accepting the
            // unchanged buffer silently.
            match Editor::new().edit(&buffer)? {
                Some(edited) => Ok(Answer::Value(edited)),
                None => Ok(Answer::Back),
            }
        })
        .await?
    }

    fn heading(&mut self, level: u8, text: &str) {
        // Render like a Markdown heading: `# Section`, `## Subsection`.
        // Section gets a horizontal rule underneath so visual separation
        // between phases is unambiguous.
        let marker = "#".repeat(level.clamp(1, 6) as usize);
        println!("\n{marker} {text}");
        if level == 1 {
            let rule_width = text.chars().count().saturating_add(2).max(20);
            println!("{}", "─".repeat(rule_width));
        }
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
