use super::agent::Agent;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use std::sync::Arc;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
use zeroclaw_memory::{Memory, MemoryCategory, SqliteMemory};

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
    let memory = SqliteMemory::new("video-test", directory.path()).unwrap();
    let recalled = format!(
        "Scene observation [VIDEO:data:video/mp4;base64,{}]",
        STANDARD.encode(b"\0\0\0\x10ftypisomtest")
    );
    for (key, note) in [
        ("scene-label", "Scene observation uses the east camera."),
        ("scene-media", recalled.as_str()),
    ] {
        memory
            .store(key, note, MemoryCategory::Core, None)
            .await
            .unwrap();
    }
    let question = "Describe the scene observation";
    let with_clip = format!("{question} [VIDEO:{}]", video.display());
    let inject = super::memory_inject::MemoryInjectConfig {
        min_relevance_score: 0.0,
        ..Default::default()
    };
    for query in [question, with_clip.as_str()] {
        let candidates = memory
            .recall(query, inject.limit, None, None, None)
            .await
            .unwrap();
        assert!(candidates.iter().any(|entry| entry.key == "scene-media"));
    }
    let mut agent = Agent::builder()
        .model_provider(provider)
        .model_name("qwen3.8-flash".into())
        .model_provider_name("qwen.test".into())
        .memory(Arc::new(memory))
        .auto_save(false)
        .memory_inject_cfg(inject)
        .observer(Arc::new(crate::observability::NoopObserver {}))
        .tools(crate::tools::scoped::ScopedToolRegistry::from_raw_for_test(
            Vec::new(),
        ))
        .tool_dispatcher(Box::new(super::dispatcher::NativeToolDispatcher))
        .workspace_dir(directory.path().to_path_buf())
        .security(Arc::new(policy))
        .build()
        .unwrap();
    agent.turn(question).await.unwrap();
    assert_eq!(agent.turn(&with_clip).await.unwrap(), "video observed");
    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        let wire = std::str::from_utf8(&request.body).unwrap();
        assert!(wire.contains("Scene observation uses the east camera."));
        assert!(!wire.contains("scene-media"));
    }
    let first: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert!(
        first["messages"]
            .as_array()
            .unwrap()
            .iter()
            .all(|message| message["content"].is_string())
    );
    let body: serde_json::Value = serde_json::from_slice(&requests[1].body).unwrap();
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
