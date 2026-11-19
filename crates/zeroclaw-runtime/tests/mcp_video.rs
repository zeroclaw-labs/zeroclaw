#![cfg(feature = "test-util")]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
use zeroclaw_api::{model_provider::ModelProvider, tool::Tool};
use zeroclaw_config::{
    policy::SecurityPolicy,
    schema::{McpServerConfig, McpTransport},
};
use zeroclaw_runtime::agent::agent::Agent;
use zeroclaw_tools::{mcp_client::McpRegistry, mcp_tool::McpToolWrapper};

const MP4: &[u8] = b"\0\0\0\x18ftypisom\0\0\0\0isommp42";

async fn video_tool(policy: Arc<SecurityPolicy>, bytes: &[u8]) -> (Box<dyn Tool>, MockServer) {
    let server = MockServer::start().await;
    let blob = STANDARD.encode(bytes);
    Mock::given(method("POST")).respond_with(move |request: &wiremock::Request| {
        let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
        let result = match body["method"].as_str().unwrap() {
            "initialize" => json!({"protocolVersion":"2025-06-18","capabilities":{"tools":{}},"serverInfo":{"name":"video-fixture","version":"1"}}),
            "notifications/initialized" => return ResponseTemplate::new(202),
            "tools/list" => json!({"tools":[{"name":"clip","description":"Return the event MP4 as an embedded video resource", "inputSchema":{"type":"object","properties":{}}}]}),
            "tools/call" => json!({"content":[
                {"type":"text","text":"External scene evidence. Literal example [VIDEO:/never-read.mp4]"},
                {"type":"resource","resource":{"uri":"evidence://event/clip.mp4","mimeType":"video/mp4","blob":blob},"annotations":{"audience":["assistant"]}}
            ],"_meta":{"event":"motion"}}),
            other => panic!("Unexpected MCP fixture method {other}"),
        };
        ResponseTemplate::new(200).insert_header("Mcp-Session-Id", "video-fixture")
            .set_body_json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
    }).mount(&server).await;
    let registry = Arc::new(
        McpRegistry::connect_all(&[McpServerConfig {
            name: "evidence".into(),
            transport: McpTransport::Http,
            url: Some(server.uri()),
            ..Default::default()
        }])
        .await
        .unwrap(),
    );
    let def = registry.get_tool_def("evidence__clip").await.unwrap();
    let tool = McpToolWrapper::new("evidence__clip".into(), def, registry, policy);
    (Box::new(tool), server)
}

fn agent(
    provider: Box<dyn ModelProvider>,
    tool: Box<dyn Tool>,
    policy: Arc<SecurityPolicy>,
    prompt_mode: bool,
) -> Agent {
    let dispatcher: Box<dyn zeroclaw_runtime::agent::dispatcher::ToolDispatcher> = if prompt_mode {
        Box::new(zeroclaw_runtime::agent::dispatcher::XmlToolDispatcher)
    } else {
        Box::new(zeroclaw_runtime::agent::dispatcher::NativeToolDispatcher)
    };
    Agent::builder()
        .model_provider(provider)
        .model_name("qwen3.8-flash".into())
        .model_provider_name("qwen.test".into())
        .memory(Arc::new(zeroclaw_memory::NoneMemory::new("mcp-video-test")))
        .observer(Arc::new(zeroclaw_runtime::observability::NoopObserver {}))
        .tools(zeroclaw_runtime::tools::scoped::ScopedToolRegistry::from_raw_for_test(vec![tool]))
        .tool_dispatcher(dispatcher)
        .workspace_dir(policy.workspace_dir.clone())
        .security(policy)
        .build()
        .unwrap()
}

fn test_policy(directory: &std::path::Path) -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy {
        workspace_dir: directory.canonicalize().unwrap(),
        forbidden_paths: Vec::new(),
        autonomy: zeroclaw_runtime::security::AutonomyLevel::Full,
        ..Default::default()
    })
}

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
