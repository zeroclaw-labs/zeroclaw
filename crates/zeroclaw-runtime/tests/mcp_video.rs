#![cfg(feature = "test-util")]

#[path = "support/mcp_video.rs"]
mod mcp_video_fixture;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use mcp_video_fixture::{agent, test_policy, video_tool};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};

const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

#[tokio::test]
async fn mcp_video_reaches_qwen_http_through_native_and_xml_agent_turns() {
    for prompt_mode in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let policy = test_policy(directory.path());
        let (tool, mcp) = video_tool(policy.clone(), MP4).await;
        let provider_http = MockServer::start().await;
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        Mock::given(method("POST")).and(path("/chat/completions"))
            .respond_with(move |_: &wiremock::Request| {
                let message = if counter.fetch_add(1, Ordering::SeqCst) == 0 {
                    if prompt_mode {
                        json!({"role":"assistant","content":"<tool_call>{\"name\":\"evidence__clip\",\"arguments\":{}}</tool_call>"})
                    } else {
                        json!({"role":"assistant","content":null,"tool_calls":[{"id":"call-video","type":"function","function":{"name":"evidence__clip","arguments":"{}"}}]})
                    }
                } else { json!({"role":"assistant","content":"People are moving through the scene."}) };
                ResponseTemplate::new(200).set_body_json(json!({"choices":[{"message":message}]}))
            }).mount(&provider_http).await;
        let options = zeroclaw_providers::ModelProviderRuntimeOptions {
            provider_api_url: Some(provider_http.uri()),
            native_tools: Some(!prompt_mode),
            ..Default::default()
        };
        let provider = zeroclaw_providers::create_model_provider_with_options(
            "qwen",
            Some("test-only"),
            &options,
        )
        .unwrap();
        let mut agent = agent(provider, tool, policy, prompt_mode);
        let answer = agent
            .turn("Obtain the event clip and describe visible motion")
            .await
            .unwrap();
        assert!(answer.contains("People"), "{answer}");
        agent
            .turn("Continue from the textual conclusion")
            .await
            .unwrap();
        let requests = provider_http.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3, "unexpected model dispatch count");
        for (index, request) in requests.iter().enumerate() {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let videos: Vec<_> = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|m| m["content"].as_array())
                .flatten()
                .filter(|p| p["type"] == "video_url")
                .collect();
            assert_eq!(
                videos.len(),
                usize::from(index == 1),
                "dispatch {index}: {body}"
            );
            if index == 1 {
                let data = videos[0]["video_url"]["url"]
                    .as_str()
                    .unwrap()
                    .strip_prefix("data:video/mp4;base64,")
                    .unwrap();
                assert_eq!(STANDARD.decode(data).unwrap(), MP4);
                assert!(body.to_string().contains("[VIDEO:/never-read.mp4]"));
                if prompt_mode {
                    assert!(body.to_string().contains("[Tool results]"), "{body}");
                }
                for message in body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["role"] == "tool")
                {
                    assert!(message["content"].is_string(), "{message}");
                }
            }
        }
        let calls = mcp
            .received_requests()
            .await
            .unwrap()
            .into_iter()
            .filter(|r| {
                let body: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
                body["method"] == "tools/call"
            })
            .count();
        assert_eq!(calls, 1);
        let files: Vec<_> = std::fs::read_dir(directory.path().join("uploads"))
            .unwrap()
            .collect();
        assert_eq!(files.len(), 1);
    }
}
