use crate::mcp_video_fixture::{agent, test_policy, video_tool};
use serde_json::json;

#[tokio::test]
#[ignore = "requires explicit Qwen credentials and a local MP4; makes paid model requests"]
async fn mcp_video_agent_to_real_qwen() {
    let key = std::env::var("ZEROCLAW_TEST_QWEN_API_KEY").expect("explicit test credential");
    let endpoint = std::env::var("ZEROCLAW_TEST_QWEN_ENDPOINT").expect("explicit test endpoint");
    let video = std::env::var("ZEROCLAW_TEST_QWEN_VIDEO").expect("explicit local MP4 fixture");
    let bytes = std::fs::read(video).unwrap();
    let directory = tempfile::tempdir().unwrap();
    let policy = test_policy(directory.path());
    let (tool, mcp) = video_tool(policy.clone(), &bytes).await;
    let options = zeroclaw_providers::ModelProviderRuntimeOptions {
        provider_api_url: Some(endpoint),
        provider_max_tokens: Some(400),
        provider_timeout_secs: Some(120),
        provider_extra: Some(json!({"enable_thinking":false})),
        ..Default::default()
    };
    let provider =
        zeroclaw_providers::create_model_provider_with_options("qwen", Some(&key), &options)
            .unwrap();
    let mut agent = agent(provider, tool, policy, false);
    let started = std::time::Instant::now();
    let answer = agent.turn("Call evidence__clip exactly once to obtain the event video, then describe the visible pedestrians and their motion in English based on that video.").await.unwrap();
    let lower = answer.to_ascii_lowercase();
    assert!(
        lower.contains("person") || lower.contains("people") || lower.contains("pedestrian"),
        "{answer}"
    );
    assert!(
        !lower.contains("cannot access") && !lower.contains("can't access"),
        "{answer}"
    );
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
    assert_eq!(calls, 1, "model must obtain the video through MCP");
    println!(
        "MCP -> Agent -> Qwen video elapsed={:?}\nanswer={answer}",
        started.elapsed()
    );
}
