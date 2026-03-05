use std::collections::HashMap;

use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zeroclaw::config::ReliabilityConfig;
use zeroclaw::providers::create_resilient_provider;

#[tokio::test]
async fn fallback_api_keys_support_multiple_custom_endpoints() {
    let primary_server = MockServer::start().await;
    let fallback_server_one = MockServer::start().await;
    let fallback_server_two = MockServer::start().await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(serde_json::json!({ "error": "primary unavailable" })),
        )
        .expect(1)
        .mount(&primary_server)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer fallback-key-1"))
        .respond_with(
            ResponseTemplate::new(500)
                .set_body_json(serde_json::json!({ "error": "fallback one unavailable" })),
        )
        .expect(1)
        .mount(&fallback_server_one)
        .await;

    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .and(header("authorization", "Bearer fallback-key-2"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "id": "chatcmpl-1",
            "object": "chat.completion",
            "choices": [
                {
                    "index": 0,
                    "message": {
                        "role": "assistant",
                        "content": "response-from-fallback-two"
                    },
                    "finish_reason": "stop"
                }
            ],
            "usage": {
                "prompt_tokens": 1,
                "completion_tokens": 1,
                "total_tokens": 2
            }
        })))
        .expect(1)
        .mount(&fallback_server_two)
        .await;

    let primary_provider = format!("custom:{}/v1", primary_server.uri());
    let fallback_provider_one = format!("custom:{}/v1", fallback_server_one.uri());
    let fallback_provider_two = format!("custom:{}/v1", fallback_server_two.uri());

    let mut fallback_api_keys = HashMap::new();
    fallback_api_keys.insert(fallback_provider_one.clone(), "fallback-key-1".to_string());
    fallback_api_keys.insert(fallback_provider_two.clone(), "fallback-key-2".to_string());

    let reliability = ReliabilityConfig {
        provider_retries: 0,
        provider_backoff_ms: 0,
        fallback_providers: vec![fallback_provider_one.clone(), fallback_provider_two.clone()],
        fallback_api_keys,
        api_keys: Vec::new(),
        model_fallbacks: HashMap::new(),
        channel_initial_backoff_secs: 2,
        channel_max_backoff_secs: 60,
        scheduler_poll_secs: 15,
        scheduler_retries: 2,
    };

    let provider =
        create_resilient_provider(&primary_provider, Some("primary-key"), None, &reliability)
            .expect("resilient provider should initialize");

    let reply = provider
        .chat_with_system(None, "hello", "gpt-4o-mini", 0.0)
        .await
        .expect("fallback chain should return final response");

    assert_eq!(reply, "response-from-fallback-two");

    primary_server.verify().await;
    fallback_server_one.verify().await;
    fallback_server_two.verify().await;
}
