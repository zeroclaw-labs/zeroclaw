//! Regression test for the 2026-04-23 silent-hang incident.
//!
//! A 502 from cliproxy with body
//! `{"error":"unknown provider for model anthropic/claude-sonnet-4-5"}` caused
//! the agent loop to hang for 13 hours with no error surfaced. The fix in
//! `compatible.rs` promotes non-2xx responses to structured errors (with
//! `tracing::warn!` logs) instead of swallowing them.
//!
//! This test exercises the non-streaming `Provider::chat` path against a
//! wiremock server that always returns 502. It asserts:
//! - the call returns `Err(_)` (not a silent hang)
//! - the error contains the status code (`502`)
//! - the error includes the upstream body snippet
//! - it fails fast (< 5 seconds)

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw_providers::compatible::{AuthStyle, OpenAiCompatibleProvider};
use zeroclaw_providers::{ChatMessage, ChatRequest, Provider};

#[tokio::test]
async fn non_2xx_surfaces_structured_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(502).set_body_json(serde_json::json!({
            "error": {"message": "unknown provider for model anthropic/claude-sonnet-4-5"}
        })))
        .mount(&server)
        .await;

    let provider = OpenAiCompatibleProvider::new_with_vision(
        "Test",
        &server.uri(),
        Some("test-key"),
        AuthStyle::Bearer,
        true,
    );

    let messages = vec![ChatMessage::user("hi")];
    let req = ChatRequest {
        messages: &messages,
        tools: None,
    };

    let start = std::time::Instant::now();
    let err = provider
        .chat(req, "anthropic/claude-sonnet-4-5", 0.0)
        .await
        .expect_err("non-2xx should surface as Err, not hang or succeed");
    let elapsed = start.elapsed();

    assert!(
        elapsed < std::time::Duration::from_secs(5),
        "should fail fast, not hang (took {elapsed:?})"
    );

    let msg = format!("{err:#}");
    assert!(msg.contains("502"), "error should contain status: {msg}");
    assert!(
        msg.contains("unknown provider for model"),
        "error should include upstream body: {msg}"
    );
}
