//! Consolidated live provider tests.
//!
//! All tests in this module require real external API credentials and are
//! marked with `#[ignore]`. Run with: `cargo test --test live -- --ignored`

use jhedaiclaw::providers::traits::{ChatMessage, Provider};
use jhedaiclaw::providers::ProviderRuntimeOptions;

/// Sends a real multi-turn conversation to OpenAI Codex and verifies
/// the model retains context from earlier messages.
///
/// Requires valid OAuth credentials in `~/.jhedaiclaw/`.
/// Run manually: `cargo test e2e_live_openai_codex_multi_turn -- --ignored`
#[tokio::test]
#[ignore = "requires live OpenAI Codex OAuth credentials"]
async fn e2e_live_openai_codex_multi_turn() {
    use jhedaiclaw::providers::openai_codex::OpenAiCodexProvider;

    let provider = OpenAiCodexProvider::new(&ProviderRuntimeOptions::default(), None).unwrap();
    let model = "gpt-5.3-codex";

    // Turn 1: establish a fact
    let messages_turn1 = vec![
        ChatMessage::system("You are a concise assistant. Reply in one short sentence."),
        ChatMessage::user("The secret word is \"zephyr\". Just confirm you noted it."),
    ];
    let response1 = provider
        .chat_with_history(&messages_turn1, model, 0.0)
        .await;
    assert!(response1.is_ok(), "Turn 1 failed: {:?}", response1.err());
    let r1 = response1.unwrap();
    assert!(!r1.is_empty(), "Turn 1 returned empty response");

    // Turn 2: ask the model to recall the fact
    let messages_turn2 = vec![
        ChatMessage::system("You are a concise assistant. Reply in one short sentence."),
        ChatMessage::user("The secret word is \"zephyr\". Just confirm you noted it."),
        ChatMessage::assistant(&r1),
        ChatMessage::user("What is the secret word?"),
    ];
    let response2 = provider
        .chat_with_history(&messages_turn2, model, 0.0)
        .await;
    assert!(response2.is_ok(), "Turn 2 failed: {:?}", response2.err());
    let r2 = response2.unwrap().to_lowercase();
    assert!(
        r2.contains("zephyr"),
        "Model should recall 'zephyr' from history, got: {r2}",
    );
}
