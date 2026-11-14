//! One chat completion through the typed `llmtr` provider.
//!
//! `LLMTR_API_KEY` is passed to `create_model_provider_with_options` as the
//! credential override. This process does not retry, fail over, attach tools,
//! or send a reasoning object. `max_tokens` defaults to 64 so the call does not
//! inherit the catalog completion cap.

use std::env;
use std::process::ExitCode;

use zeroclaw_providers::traits::ModelProvider;
use zeroclaw_providers::{ModelProviderRuntimeOptions, create_model_provider_with_options};

const DEFAULT_MAX_TOKENS: u32 = 64;

fn usage() -> &'static str {
    "\
llmtr-chat: one LLMTR chat completion, no tools, no retries, no failover.

Usage:
  LLMTR_API_KEY=... llmtr-chat [--model MODEL] [--max-tokens N] [PROMPT]

Defaults:
  model       minimax/minimax-m3.1-flash
  max-tokens  64
  prompt      Reply with ok.

The model id is sent verbatim. Chat uses POST /v1/chat/completions only.
A reasoning object is not sent; the server may still reason.
GET /v1/models is not called and is not a key check.
"
}

#[tokio::main]
async fn main() -> ExitCode {
    match run().await {
        Ok(Some(text)) => {
            println!("{text}");
            ExitCode::SUCCESS
        }
        Ok(None) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("llmtr-chat: {error:#}");
            ExitCode::from(1)
        }
    }
}

async fn run() -> anyhow::Result<Option<String>> {
    let mut model = zeroclaw_config::schema::LLMTR_EXAMPLE_MODEL.to_string();
    let mut max_tokens = DEFAULT_MAX_TOKENS;
    let mut prompt: Option<String> = None;
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--help" | "-h" => {
                eprint!("{}", usage());
                return Ok(None);
            }
            "--model" => {
                model = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--model needs a value"))?;
            }
            "--max-tokens" => {
                let raw = args
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("--max-tokens needs a value"))?;
                max_tokens = raw
                    .parse()
                    .map_err(|_| anyhow::anyhow!("--max-tokens must be an integer"))?;
            }
            other if other.starts_with('-') => {
                anyhow::bail!("unknown argument {other}\n{}", usage());
            }
            other => {
                if prompt.is_some() {
                    anyhow::bail!("only one prompt is accepted");
                }
                prompt = Some(other.to_string());
            }
        }
    }
    if model.trim().is_empty() {
        anyhow::bail!("model id is empty");
    }
    let prompt = prompt.unwrap_or_else(|| "Reply with ok.".to_string());
    let key = env::var("LLMTR_API_KEY")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow::anyhow!("LLMTR_API_KEY is required"))?;

    eprintln!(
        "llmtr-chat: one chat completion via the llmtr provider, no tools, no retries, no failover, model={model}, max_tokens={max_tokens}"
    );

    let provider = create_model_provider_with_options(
        "llmtr",
        Some(key.as_str()),
        &ModelProviderRuntimeOptions {
            provider_max_tokens: Some(max_tokens),
            ..ModelProviderRuntimeOptions::default()
        },
    )?;
    provider
        .simple_chat(&prompt, model.trim(), None)
        .await
        .map(Some)
}
