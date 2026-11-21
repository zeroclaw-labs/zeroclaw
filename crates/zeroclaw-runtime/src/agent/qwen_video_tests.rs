use super::agent::Agent;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

#[tokio::test]
async fn agent_turn_applies_its_filesystem_policy_to_qwen_video() {
    let directory = tempfile::tempdir().unwrap();
    let video = directory.path().join("clip.mp4");
    let bytes = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";
    std::fs::write(&video, bytes).unwrap();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "choices": [{"message": {"role": "assistant", "content": "video observed"}}]
        })))
        .mount(&server)
        .await;
    let options = zeroclaw_providers::ModelProviderRuntimeOptions {
        provider_api_url: Some(server.uri()),
        ..Default::default()
    };
    let provider =
        zeroclaw_providers::create_model_provider_with_options("qwen", Some("test-only"), &options)
            .unwrap();
    let policy = zeroclaw_config::policy::SecurityPolicy {
        workspace_dir: directory.path().to_path_buf(),
        forbidden_paths: Vec::new(),
        ..Default::default()
    };
    let mut agent = Agent::builder()
        .model_provider(provider)
        .model_name("qwen3.8-flash".into())
        .model_provider_name("qwen.test".into())
        .memory(Arc::new(zeroclaw_memory::NoneMemory::new("video-test")))
        .observer(Arc::new(crate::observability::NoopObserver {}))
        .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
            Vec::new(),
        ))
        .tool_dispatcher(Box::new(super::dispatcher::NativeToolDispatcher))
        .workspace_dir(directory.path().to_path_buf())
        .security(Arc::new(policy))
        .build()
        .unwrap();
    assert_eq!(
        agent
            .turn(&format!("Describe [VIDEO:{}]", video.display()))
            .await
            .unwrap(),
        "video observed"
    );
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 1);
    let body: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    let videos: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter(|part| part["type"] == "video_url")
        .collect();
    assert_eq!(videos.len(), 1);
    let data = videos[0]["video_url"]["url"]
        .as_str()
        .unwrap()
        .strip_prefix("data:video/mp4;base64,")
        .unwrap();
    assert_eq!(STANDARD.decode(data).unwrap(), bytes);
}
