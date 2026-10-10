//! Real public-provider/factory and runtime-loop proof with a local tool fixture.
use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio_util::sync::CancellationToken;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
};
use zeroclaw_api::{
    model_provider::ChatMessage,
    tool::{Tool, ToolResult},
};
use zeroclaw_runtime::{
    agent::loop_::{
        LoopKnobs, ResolvedAgentExecution, ResolvedModelAccess, ToolLoop, run_tool_call_loop,
    },
    approval::ApprovalManager,
    tools::scoped::ScopedToolRegistry,
};

struct LocalActionFixture {
    calls: Arc<AtomicUsize>,
    cancellation: Option<CancellationToken>,
}
zeroclaw_api::mock_tool_attribution!(LocalActionFixture);
#[async_trait::async_trait]
impl Tool for LocalActionFixture {
    fn name(&self) -> &str {
        "fixture_action"
    }
    fn description(&self) -> &str {
        "Count a local fixture invocation"
    }
    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"command":{"type":"string"}},"required":["command"]})
    }
    async fn execute(&self, _args: serde_json::Value) -> anyhow::Result<ToolResult> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(token) = &self.cancellation {
            token.cancel();
        }
        Ok(ToolResult {
            success: true,
            output: "local fixture executed".into(),
            error: None,
        })
    }
}

async fn run_chatgpt_plan_loop_fixture(preset: &str, cancel: bool) {
    use zeroclaw_providers::auth::profiles::{
        AuthProfile, AuthProfilesStore, ChatGptPlanRegistration, TokenSet,
    };
    let root = tempfile::tempdir().unwrap();
    let mut profile = AuthProfile::new_oauth(
        "chatgpt-plan",
        "subscriber",
        TokenSet {
            access_token: "synthetic-loop-access".into(),
            refresh_token: Some("synthetic-loop-refresh".into()),
            id_token: None,
            expires_at: Some(chrono::Utc::now() + chrono::Duration::hours(1)),
            token_type: Some("Bearer".into()),
            scope: Some("chatgpt.tokens.use.direct".into()),
        },
    );
    profile.plan_registration = Some(ChatGptPlanRegistration {
        client_id: "oaiapp_fixture".into(),
        subject: "subject-fixture".into(),
        earliest_refresh_at: None,
        refresh_started_at: None,
    });
    AuthProfilesStore::new(root.path(), true)
        .upsert_profile(profile, false)
        .await
        .unwrap();
    let server = MockServer::start().await;
    let requests = Arc::new(Mutex::new(Vec::new()));
    Mock::given(method("POST")).and(path("/responses")).respond_with({
        let requests = requests.clone();
        move |request: &wiremock::Request| {
            assert_eq!(request.headers.get("authorization").unwrap(), "Bearer synthetic-loop-access");
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["tools"][0]["type"], "namespace");
            assert_eq!(body["tools"][0]["name"], "zeroclaw");
            assert_eq!(body["tools"][0]["tools"][0]["name"], "fixture_action");
            assert_eq!(body["store"], false);
            assert_eq!(body["stream"], true);
            let followup = body["input"].as_array().unwrap().iter().any(|item| item["type"] == "function_call_output");
            requests.lock().unwrap().push(body);
            let output = if followup { serde_json::json!([{"type":"message","role":"assistant","content":[{"type":"output_text","text":"LOOP COMPLETE"}]}]) } else {
                let mut calls = vec![serde_json::json!({"type":"function_call","namespace":"zeroclaw","name":"fixture_action","call_id":"call_first","arguments":"{\"command\":\"fixture\"}","status":"completed"})];
                if cancel { calls.push(serde_json::json!({"type":"function_call","namespace":"zeroclaw","name":"fixture_action","call_id":"call_tail","arguments":"{\"command\":\"tail\"}","status":"completed"})); }
                serde_json::Value::Array(calls)
            };
            ResponseTemplate::new(200).set_body_raw(format!("data: {}\n\n",serde_json::json!({"type":"response.completed","response":{"status":"completed","output":output}})), "text/event-stream")
        }
    }).mount(&server).await;
    let mut config = Config {
        config_path: root.path().join("config.toml"),
        ..Default::default()
    };
    config.providers.models.openai.insert(
        "subscriber".into(),
        zeroclaw_config::schema::OpenAIModelProviderConfig {
            base: zeroclaw_config::schema::ModelProviderConfig {
                kind: Some("chatgpt-plan".into()),
                model: Some("model-fixture".into()),
                chatgpt_plan_auth: Some(zeroclaw_config::schema::ChatGptPlanAuthConfig {
                    registration: "chatgpt-plan:subscriber".into(),
                }),
                ..Default::default()
            },
        },
    );
    let risk = (zeroclaw_config::presets::risk_preset(preset)
        .unwrap()
        .values)();
    let approval = ApprovalManager::for_non_interactive(&risk);
    let calls = Arc::new(AtomicUsize::new(0));
    let token = CancellationToken::new();
    let registry = ScopedToolRegistry::from_raw_for_test(vec![Box::new(LocalActionFixture {
        calls: calls.clone(),
        cancellation: cancel.then(|| token.clone()),
    })]);
    let observer = zeroclaw_runtime::observability::NoopObserver;
    let mut history = vec![
        ChatMessage::system("Use the local fixture"),
        ChatMessage::user("fixture request"),
    ];
    zeroclaw_providers::plan_test_transport::scope(&server.uri(), async {
        let opts = zeroclaw_providers::model_provider_runtime_options_from_model_provider_entry(
            &config,
            config.providers.models.find("openai", "subscriber"),
        );
        let provider = zeroclaw_providers::create_model_provider_for_alias(
            &config,
            "openai",
            "subscriber",
            None,
            &opts,
        )
        .unwrap();
        let result = Box::pin(run_tool_call_loop(ToolLoop {
            exec: ResolvedAgentExecution {
                model_access: ResolvedModelAccess {
                    model_provider: provider.as_ref(),
                    provider_name: "openai.subscriber",
                    model: "model-fixture",
                    dispatch_model: "model-fixture",
                    temperature: None,
                },
                tools_registry: &registry,
                observer: &observer,
                silent: true,
                approval: Some(&approval),
                security: None,
                multimodal_config: &zeroclaw_config::schema::MultimodalConfig::default(),
                config: Some(&config),
                max_tool_iterations: 3,
                hooks: None,
                excluded_tools: &[],
                dedup_exempt_tools: &[],
                activated_tools: None,
                model_switch_callback: None,
                pacing: &zeroclaw_config::schema::PacingConfig::default(),
                strict_tool_parsing: true,
                parallel_tools: false,
                max_tool_result_chars: 0,
                context_limits: zeroclaw_config::schema::ResolvedContextLimits::legacy_fallback(0),
                context_limits_resolver: None,
                receipt_generator: None,
                knobs: &LoopKnobs::default(),
            },
            history: &mut history,
            history_has_trim_breadcrumb: &mut false,
            injected_memory_preamble: &mut None,
            channel_name: "cli",
            channel_reply_target: None,
            cancellation_token: Some(token.clone()),
            on_delta: None,
            shared_budget: None,
            channel: None,
            collected_receipts: None,
            event_tx: None,
            steering: None,
            new_messages_out: None,
            image_cache: None,
            ingress: zeroclaw_api::ingress::IngressContext::sub_turn(),
            memory: None,
            agent_alias: Some("fixture"),
            parent_agent_alias: None,
            turn_id: "fixture-turn",
            served_route_sink: None,
            sop_reassembly: None,
        }))
        .await;
        if cancel {
            assert!(result.is_err());
        } else {
            assert_eq!(result.unwrap(), "LOOP COMPLETE");
        }
    })
    .await;
    let requests = requests.lock().unwrap();
    if cancel {
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "tail must not dispatch after cancellation"
        );
        assert_eq!(
            requests.len(),
            1,
            "cancelled turn must not continue inference"
        );
    } else {
        assert_eq!(requests.len(), 2);
        let output = requests[1]["input"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["type"] == "function_call_output")
            .unwrap();
        assert_eq!(output["call_id"], "call_first");
        if preset == "yolo" {
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(
                output["output"]
                    .as_str()
                    .unwrap()
                    .contains("local fixture executed")
            );
        } else {
            assert_eq!(
                calls.load(Ordering::SeqCst),
                0,
                "prompt-required tools must not execute on a noninteractive surface"
            );
            assert!(
                !output["output"]
                    .as_str()
                    .unwrap()
                    .contains("local fixture executed")
            );
        }
    }
    let raw = std::fs::read_to_string(root.path().join("auth-profiles.json")).unwrap();
    for secret in ["synthetic-loop-access", "synthetic-loop-refresh"] {
        assert!(!raw.contains(secret));
    }
}

#[tokio::test]
async fn chatgpt_plan_runtime_loop_executes_under_canonical_yolo() {
    Box::pin(run_chatgpt_plan_loop_fixture("yolo", false)).await;
}
#[tokio::test]
async fn chatgpt_plan_runtime_loop_denies_prompt_required_local_tool() {
    Box::pin(run_chatgpt_plan_loop_fixture("locked_down", false)).await;
}
#[tokio::test]
async fn chatgpt_plan_runtime_loop_cancellation_stops_tail_and_inference() {
    Box::pin(run_chatgpt_plan_loop_fixture("yolo", true)).await;
}
