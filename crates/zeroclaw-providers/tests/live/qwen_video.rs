use zeroclaw_api::model_provider::{ChatMessage, ChatRequest as ProviderChatRequest};
use zeroclaw_providers::{ModelProviderRuntimeOptions, create_model_provider_with_options};

#[tokio::test]
#[ignore = "requires explicit Qwen credentials and a local MP4; makes a paid model request"]
async fn qwen_video_file_factory_to_real_provider() {
    let key = std::env::var("ZEROCLAW_TEST_QWEN_API_KEY").expect("explicit test credential");
    let endpoint = std::env::var("ZEROCLAW_TEST_QWEN_ENDPOINT").expect("explicit test endpoint");
    let path = std::env::var("ZEROCLAW_TEST_QWEN_VIDEO").expect("explicit local MP4 fixture");
    let model =
        std::env::var("ZEROCLAW_TEST_QWEN_MODEL").unwrap_or_else(|_| "qwen3.8-flash".into());
    let options = ModelProviderRuntimeOptions {
        provider_api_url: Some(endpoint),
        provider_max_tokens: Some(400),
        provider_timeout_secs: Some(120),
        provider_extra: Some(serde_json::json!({"enable_thinking": false})),
        ..Default::default()
    };
    let provider = create_model_provider_with_options("qwen", Some(&key), &options).unwrap();
    let messages = [ChatMessage::user(format!(
        "Describe the visible movement in this video. Do not invent hidden events. [VIDEO:{path}]"
    ))];
    let policy = zeroclaw_config::policy::SecurityPolicy {
        workspace_dir: std::path::Path::new(&path)
            .parent()
            .expect("video directory")
            .to_path_buf(),
        forbidden_paths: Vec::new(),
        ..Default::default()
    };
    let response = zeroclaw_providers::with_video_read_policy(
        Some(std::sync::Arc::new(policy)),
        provider.chat(
            ProviderChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            &model,
            Some(0.0),
        ),
    )
    .await
    .unwrap();
    let text = response.text.expect("video understanding returns text");
    let lower = text.to_ascii_lowercase();
    assert!(
        lower.contains("person") || lower.contains("people") || lower.contains("pedestrian"),
        "No visible pedestrians described: {text}"
    );
    assert!(
        !lower.contains("can't access") && !lower.contains("cannot access"),
        "Video was unavailable: {text}"
    );
    println!("model={model} usage={:?}\nanswer={text}", response.usage);
}
