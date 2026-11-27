use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::json;
use std::sync::Arc;
use wiremock::{Mock, MockServer, ResponseTemplate, matchers::method};
use zeroclaw_api::{model_provider::ModelProvider, tool::Tool};
use zeroclaw_config::{
    policy::SecurityPolicy,
    schema::{McpServerConfig, McpTransport},
};
use zeroclaw_runtime::agent::agent::Agent;
use zeroclaw_tools::{mcp_client::McpRegistry, mcp_tool::McpToolWrapper};

pub(crate) async fn video_tool(
    policy: Arc<SecurityPolicy>,
    bytes: &[u8],
) -> (Box<dyn Tool>, MockServer) {
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

pub(crate) fn agent(
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

pub(crate) fn test_policy(directory: &std::path::Path) -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy {
        workspace_dir: directory.canonicalize().unwrap(),
        forbidden_paths: Vec::new(),
        autonomy: zeroclaw_runtime::security::AutonomyLevel::Full,
        ..Default::default()
    })
}
