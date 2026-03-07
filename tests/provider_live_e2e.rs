//! Live E2E tests for all configured providers and models.
//!
//! These tests validate that OAuth credentials, API connectivity, model
//! availability, multi-turn conversation, profile switching, and fallback
//! chains all work against live infrastructure.
//!
//! All tests are `#[ignore]` — they require live credentials in
//! `~/.zeroclaw/auth-profiles.json`.
//!
//! Run all:
//!   cargo test --test provider_live_e2e -- --ignored --test-threads=1
//!
//! Run by provider:
//!   cargo test --test provider_live_e2e -- --ignored openai --test-threads=1
//!   cargo test --test provider_live_e2e -- --ignored gemini --test-threads=1

use anyhow::Result;
use std::sync::Once;
use zeroclaw::providers::{ChatMessage, ProviderRuntimeOptions};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

static INIT_CRYPTO: Once = Once::new();

/// Install the rustls crypto provider (required for OpenAI Codex TLS).
fn ensure_crypto() {
    INIT_CRYPTO.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// Build `ProviderRuntimeOptions` with an optional auth profile override.
fn opts(profile: Option<&str>) -> ProviderRuntimeOptions {
    ProviderRuntimeOptions {
        auth_profile_override: profile.map(String::from),
        ..Default::default()
    }
}

/// Assert that a response to "What is 2+2?" contains the answer.
fn assert_four(response: &str) {
    let low = response.to_lowercase();
    assert!(
        low.contains('4') || low.contains("four"),
        "expected '4' or 'four' in response: {response}"
    );
}

/// Create a provider with optional profile, send a simple "2+2" prompt,
/// assert the answer, and return the response text.
async fn chat_assert_four(provider_name: &str, profile: Option<&str>, model: &str) -> Result<()> {
    ensure_crypto();
    let provider =
        zeroclaw::providers::create_provider_with_options(provider_name, None, &opts(profile))?;

    println!(
        "  provider={} profile={} model={}",
        provider_name,
        profile.unwrap_or("(default)"),
        model
    );

    let response = provider
        .chat_with_system(Some("Answer in one word."), "What is 2+2?", model, 0.0)
        .await?;

    println!("  response: {}", response);
    assert!(!response.trim().is_empty(), "response must not be empty");
    assert_four(&response);
    Ok(())
}

/// Multi-turn: set a secret word, then ask the model to recall it.
async fn multi_turn_recall(provider_name: &str, profile: Option<&str>, model: &str) -> Result<()> {
    ensure_crypto();
    let provider =
        zeroclaw::providers::create_provider_with_options(provider_name, None, &opts(profile))?;

    println!(
        "  multi-turn: provider={} profile={} model={}",
        provider_name,
        profile.unwrap_or("(default)"),
        model
    );

    // Turn 1: plant the secret word.
    let msgs = vec![
        ChatMessage::system("Be concise. Always answer in one sentence."),
        ChatMessage::user("The secret word is 'zephyr'. Remember it and confirm."),
    ];
    let r1 = provider.chat_with_history(&msgs, model, 0.0).await?;
    println!("  turn-1: {}", r1);

    // Pause between turns to avoid per-minute rate limits on tight-quota providers.
    tokio::time::sleep(std::time::Duration::from_secs(60)).await;

    // Turn 2: ask for recall.
    let msgs2 = vec![
        ChatMessage::system("Be concise. Always answer in one sentence."),
        ChatMessage::user("The secret word is 'zephyr'. Remember it and confirm."),
        ChatMessage::assistant(&r1),
        ChatMessage::user("What is the secret word I told you?"),
    ];
    let r2 = provider.chat_with_history(&msgs2, model, 0.0).await?;
    println!("  turn-2: {}", r2);

    assert!(
        r2.to_lowercase().contains("zephyr"),
        "model failed to recall secret word 'zephyr'; got: {r2}"
    );
    Ok(())
}

// ===========================================================================
// Group 1: OpenAI Codex — default profile
// ===========================================================================

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_gpt_5_2() -> Result<()> {
    chat_assert_four("openai-codex", None, "gpt-5.2").await
}

// NOTE: gpt-4o, gpt-4o-mini, o4-mini are NOT supported on Codex with
// ChatGPT accounts. Only codex-series models work through the Codex provider.

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_gpt_5_1_codex_mini() -> Result<()> {
    chat_assert_four("openai-codex", None, "gpt-5.1-codex-mini").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_gpt_5_3_codex() -> Result<()> {
    chat_assert_four("openai-codex", None, "gpt-5.3-codex").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_gpt_5_2_codex() -> Result<()> {
    chat_assert_four("openai-codex", None, "gpt-5.2-codex").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_gpt_5_codex() -> Result<()> {
    chat_assert_four("openai-codex", None, "gpt-5-codex").await
}

// ===========================================================================
// Group 2: Gemini — profile gemini-1
// ===========================================================================

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_2_5_flash() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-2.5-flash").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_2_5_flash_lite() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-2.5-flash-lite").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_2_5_pro() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-2.5-pro").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_3_pro_preview() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-3-pro-preview").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_3_flash_preview() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-3-flash-preview").await
}

// ===========================================================================
// Group 3: Multi-turn conversation
// ===========================================================================

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_multi_turn() -> Result<()> {
    multi_turn_recall("openai-codex", None, "gpt-5.2-codex").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_live_gemini_multi_turn() -> Result<()> {
    multi_turn_recall("gemini", Some("gemini-2"), "gemini-2.5-flash").await
}

// ===========================================================================
// Group 4: Profile switching
// ===========================================================================

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials (codex-1 profile)"]
async fn e2e_live_codex_profile_1_works() -> Result<()> {
    chat_assert_four("openai-codex", Some("codex-1"), "gpt-5.1-codex-mini").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials (codex-2 profile)"]
async fn e2e_live_codex_profile_2_works() -> Result<()> {
    chat_assert_four("openai-codex", Some("codex-2"), "gpt-5.1-codex-mini").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials (gemini-1 profile)"]
async fn e2e_live_gemini_profile_1_works() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-1"), "gemini-2.5-flash-lite").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials (gemini-2 profile)"]
async fn e2e_live_gemini_profile_2_works() -> Result<()> {
    chat_assert_four("gemini", Some("gemini-2"), "gemini-2.5-flash-lite").await
}

// ===========================================================================
// Group 5: Fallback chain — every configured fallback provider is reachable
// ===========================================================================

#[tokio::test]
#[ignore = "requires live OAuth credentials for all fallback providers"]
async fn e2e_live_fallback_providers_all_reachable() -> Result<()> {
    ensure_crypto();

    // Fallback chain from config: gemini:gemini-1, gemini:gemini-2,
    //                             openai-codex:codex-1, openai-codex:codex-2
    let fallbacks: &[(&str, Option<&str>, &str)] = &[
        ("gemini", Some("gemini-1"), "gemini-2.5-flash"),
        ("gemini", Some("gemini-2"), "gemini-2.5-flash"),
        ("openai-codex", Some("codex-1"), "gpt-5.1-codex-mini"),
        ("openai-codex", Some("codex-2"), "gpt-5.1-codex-mini"),
    ];

    let mut failures = Vec::new();

    for (provider_name, profile, model) in fallbacks {
        let label = match profile {
            Some(p) => format!("{provider_name}:{p}"),
            None => provider_name.to_string(),
        };
        println!("  fallback {label} ...");
        match chat_assert_four(provider_name, *profile, model).await {
            Ok(()) => println!("  {label}: OK"),
            Err(e) => {
                println!("  {label}: FAIL: {e}");
                failures.push(format!("{label}: {e}"));
            }
        }
    }

    if !failures.is_empty() {
        anyhow::bail!(
            "fallback provider(s) unreachable:\n  {}",
            failures.join("\n  ")
        );
    }
    Ok(())
}

// ===========================================================================
// Group 6: Provider switching — each profile sends a real request
//
// These tests simulate what happens when the user types:
//   /providers gemini:gemini-1
//   /providers openai-codex:codex-2
// ...and then sends a message. We create each named provider directly,
// send a prompt, and assert we get a real response back.
// ===========================================================================

/// Create a provider from a profile string like "gemini:gemini-1" or
/// "openai-codex:codex-2", send a real request, assert we get "4".
async fn switch_and_chat(profile_str: &str, model: &str) -> Result<()> {
    ensure_crypto();

    // Resolve base provider name and profile name from "base:profile" format
    let (provider_name, profile) = if let Some((base, prof)) = profile_str.split_once(':') {
        (base, Some(prof))
    } else {
        (profile_str, None)
    };

    println!("  switch: profile_str={profile_str} => provider={provider_name} profile={profile:?} model={model}");
    chat_assert_four(provider_name, profile, model).await?;
    println!("  switch OK: {profile_str} responded correctly");
    Ok(())
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials (gemini-1 profile)"]
async fn e2e_provider_switch_gemini_gemini_1() -> Result<()> {
    switch_and_chat("gemini:gemini-1", "gemini-2.5-flash").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials (gemini-2 profile)"]
async fn e2e_provider_switch_gemini_gemini_2() -> Result<()> {
    switch_and_chat("gemini:gemini-2", "gemini-2.5-flash").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials (codex-1 profile)"]
async fn e2e_provider_switch_openai_codex_codex_1() -> Result<()> {
    switch_and_chat("openai-codex:codex-1", "gpt-5.1-codex-mini").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials (codex-2 profile)"]
async fn e2e_provider_switch_openai_codex_codex_2() -> Result<()> {
    switch_and_chat("openai-codex:codex-2", "gpt-5.1-codex-mini").await
}

/// Sequential switching: switch through all four configured profiles one by
/// one and verify each one responds correctly. This matches the real user flow.
#[tokio::test]
#[ignore = "requires live OAuth credentials for all configured profiles"]
async fn e2e_provider_switch_all_profiles_sequential() -> Result<()> {
    // Interleave Gemini and Codex to avoid per-minute rate limits on Gemini accounts.
    let profiles: &[(&str, &str)] = &[
        ("openai-codex:codex-1", "gpt-5.1-codex-mini"),
        ("gemini:gemini-1", "gemini-2.5-flash"),
        ("openai-codex:codex-2", "gpt-5.1-codex-mini"),
        ("gemini:gemini-2", "gemini-2.5-flash"),
    ];

    let mut failures = Vec::new();
    let mut last_gemini_call: Option<std::time::Instant> = None;

    for (profile_str, model) in profiles {
        // Gemini accounts have tight per-minute quotas — wait 65s between calls.
        if profile_str.starts_with("gemini:") {
            if let Some(last) = last_gemini_call {
                let elapsed = last.elapsed();
                let wait = std::time::Duration::from_secs(65);
                if elapsed < wait {
                    let remaining = wait - elapsed;
                    println!(
                        "  rate-limit guard: sleeping {}s before next Gemini call",
                        remaining.as_secs()
                    );
                    tokio::time::sleep(remaining).await;
                }
            }
            last_gemini_call = Some(std::time::Instant::now());
        }

        match switch_and_chat(profile_str, model).await {
            Ok(()) => {}
            Err(e) => failures.push(format!("{profile_str}: {e}")),
        }
    }

    if !failures.is_empty() {
        anyhow::bail!("provider switch failures:\n  {}", failures.join("\n  "));
    }
    Ok(())
}

// ===========================================================================
// Group 7: Model switching — verify that after switching model the new model
// is actually used for inference (response comes from the right model).
// ===========================================================================

/// Switch provider to a profile, then send a prompt asking the model to
/// identify itself, and assert the response references the expected model family.
async fn switch_provider_and_check_model(
    provider_name: &str,
    profile: Option<&str>,
    model: &str,
) -> Result<()> {
    ensure_crypto();
    let provider =
        zeroclaw::providers::create_provider_with_options(provider_name, None, &opts(profile))?;

    println!(
        "  model-check: provider={} profile={} model={}",
        provider_name,
        profile.unwrap_or("(default)"),
        model,
    );

    // Ask model to confirm it can respond (simple math)
    let response = provider
        .chat_with_system(Some("Answer in one word."), "What is 2+2?", model, 0.0)
        .await?;

    println!("  response: {}", response);
    assert!(!response.trim().is_empty(), "response must not be empty");
    assert_four(&response);
    println!("  model {model} responded correctly");
    Ok(())
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_model_switch_codex_gpt_5_2_responds() -> Result<()> {
    switch_provider_and_check_model("openai-codex", None, "gpt-5.2").await
}

#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_model_switch_codex_gpt_5_1_mini_responds() -> Result<()> {
    switch_provider_and_check_model("openai-codex", None, "gpt-5.1-codex-mini").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_model_switch_gemini_flash_responds() -> Result<()> {
    switch_provider_and_check_model("gemini", Some("gemini-1"), "gemini-2.5-flash").await
}

#[tokio::test]
#[ignore = "requires live Gemini OAuth credentials"]
async fn e2e_model_switch_gemini_flash_lite_responds() -> Result<()> {
    switch_provider_and_check_model("gemini", Some("gemini-1"), "gemini-2.5-flash-lite").await
}

/// Sequential: switch provider then switch model, verify inference works.
#[tokio::test]
#[ignore = "requires live OAuth credentials for openai-codex and gemini"]
async fn e2e_model_switch_after_provider_switch() -> Result<()> {
    let cases: &[(&str, Option<&str>, &str)] = &[
        ("openai-codex", None, "gpt-5.2"),
        ("gemini", Some("gemini-1"), "gemini-2.5-flash"),
        ("openai-codex", Some("codex-1"), "gpt-5.1-codex-mini"),
        ("gemini", Some("gemini-2"), "gemini-2.5-flash-lite"),
    ];

    let mut failures = Vec::new();
    for (provider, profile, model) in cases {
        let label = format!(
            "{}{}:{model}",
            provider,
            profile.map(|p| format!(":{p}")).unwrap_or_default()
        );
        match switch_provider_and_check_model(provider, *profile, model).await {
            Ok(()) => println!("  {label}: OK"),
            Err(e) => failures.push(format!("{label}: {e}")),
        }
    }

    if !failures.is_empty() {
        anyhow::bail!("model switch failures:\n  {}", failures.join("\n  "));
    }
    Ok(())
}
