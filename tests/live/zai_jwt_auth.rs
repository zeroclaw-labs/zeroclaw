//! Live test for Z.AI JWT authentication.
//!
//! Verifies that the ZhipuJwt auth style correctly generates a JWT token
//! and authenticates against the real Z.AI API.
//!
//! Requires `ZAI_API_KEY` env var set (format: `id.secret`).
//! Run: `ZAI_API_KEY=... cargo test live_zai -- --ignored --nocapture`

use zeroclaw::providers::create_provider;
use zeroclaw::providers::traits::ChatMessage;

/// Near-zero temperature for the single-word sanity check; we ask for "one
/// word" and just assert the response is non-empty, so a near-deterministic
/// value avoids verbose sampling artifacts.
const ZAI_SANITY_TEMPERATURE: f64 = 0.1;

/// Zero = greedy sampling; the multi-turn test asserts the exact secret
/// word ("banana") appears in the reply, so determinism is required.
const ZAI_RECALL_TEMPERATURE: f64 = 0.0;

/// Sends a simple chat request to Z.AI with JWT auth and verifies a 200 response.
#[tokio::test]
#[ignore = "requires live ZAI_API_KEY"]
async fn live_zai_jwt_auth_chat() {
    let key = std::env::var("ZAI_API_KEY").expect("ZAI_API_KEY must be set");
    let provider = create_provider("zai", Some(&key)).expect("should create ZAI provider");

    let result = provider
        .chat_with_system(
            Some("Reply in exactly one word."),
            "What color is the sky?",
            "glm-5-turbo",
            Some(ZAI_SANITY_TEMPERATURE),
        )
        .await;

    match &result {
        Ok(response) => {
            println!("[ZAI live] Response: {response}");
            assert!(!response.is_empty(), "response should not be empty");
        }
        Err(e) => {
            panic!("[ZAI live] Request failed: {e}");
        }
    }
}

/// Sends a multi-turn conversation to Z.AI to verify history works with JWT auth.
#[tokio::test]
#[ignore = "requires live ZAI_API_KEY"]
async fn live_zai_jwt_auth_multi_turn() {
    let key = std::env::var("ZAI_API_KEY").expect("ZAI_API_KEY must be set");
    let provider = create_provider("zai", Some(&key)).expect("should create ZAI provider");

    let messages = vec![
        ChatMessage::system("You are a concise assistant. Reply in one short sentence."),
        ChatMessage::user("The secret word is 'banana'. Confirm you noted it."),
        ChatMessage::assistant("Noted: the secret word is banana."),
        ChatMessage::user("What is the secret word?"),
    ];

    let result = provider
        .chat_with_history(&messages, "glm-5-turbo", Some(ZAI_RECALL_TEMPERATURE))
        .await;

    match &result {
        Ok(response) => {
            println!("[ZAI live multi-turn] Response: {response}");
            let lower = response.to_lowercase();
            assert!(
                lower.contains("banana"),
                "model should recall 'banana', got: {response}"
            );
        }
        Err(e) => {
            panic!("[ZAI live multi-turn] Request failed: {e}");
        }
    }
}
