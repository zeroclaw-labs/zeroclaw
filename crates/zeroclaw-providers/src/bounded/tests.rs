use axum::{
    Router,
    body::Bytes,
    http::{StatusCode, Uri},
    response::IntoResponse,
};
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::Notify;
use zeroclaw_api::model_provider::{
    ChatMessage, ChatRequest, GenerationAttemptBudget, GenerationAttemptBudgetExhausted,
    ModelProvider, ToolRoundPolicy,
};

tokio::task_local! { static ENDPOINT: String; }

// Fixed-endpoint adapters retain request construction and codecs while their
// generation transport targets loopback. Hailo uses its configured URL.
pub(crate) fn redirect_request(mut request: reqwest::Request) -> reqwest::Request {
    if let Ok(endpoint) = ENDPOINT.try_with(Clone::clone) {
        let mut url = reqwest::Url::parse(&endpoint).unwrap();
        url.set_path(request.url().path());
        url.set_query(request.url().query());
        *request.url_mut() = url;
    }
    request
}

pub(crate) async fn at_endpoint<F: Future>(url: &str, future: F) -> F::Output {
    ENDPOINT.scope(url.to_owned(), future).await
}

pub(crate) struct Reply {
    status: StatusCode,
    body: String,
    content_type: &'static str,
    hold: bool,
}
impl Reply {
    pub(crate) fn json(status: u16, body: &str) -> Self {
        Self {
            status: StatusCode::from_u16(status).unwrap(),
            body: body.to_owned(),
            content_type: "application/json",
            hold: false,
        }
    }
    pub(crate) fn stream(body: &str) -> Self {
        Self {
            content_type: "text/event-stream",
            ..Self::json(200, body)
        }
    }
    pub(crate) fn held(mut self) -> Self {
        self.hold = true;
        self
    }
}

pub(crate) struct Server {
    pub(crate) url: String,
    pub(crate) requests: Arc<Mutex<Vec<(String, serde_json::Value)>>>,
    pub(crate) received: Arc<Notify>,
    pub(crate) release: Arc<Notify>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn server(replies: Vec<Reply>) -> Server {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let queue = Arc::new(Mutex::new(VecDeque::from(replies)));
    let received = Arc::new(Notify::new());
    let release = Arc::new(Notify::new());
    let app = Router::new().fallback({
        let requests = requests.clone();
        let received = received.clone();
        let release = release.clone();
        move |uri: Uri, body: Bytes| {
            let requests = requests.clone();
            let queue = queue.clone();
            let received = received.clone();
            let release = release.clone();
            async move {
                requests.lock().push((
                    uri.to_string(),
                    serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null),
                ));
                received.notify_one();
                let reply = queue
                    .lock()
                    .pop_front()
                    .unwrap_or_else(|| Reply::json(500, "{}"));
                if reply.hold {
                    release.notified().await;
                }
                (
                    reply.status,
                    [("content-type", reply.content_type)],
                    reply.body,
                )
                    .into_response()
            }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let task = ::zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
    Server {
        url,
        requests,
        received,
        release,
        task,
    }
}

pub(crate) const CHAT_OK: &str = r#"{"choices":[{"message":{"role":"assistant","content":"ok"}}]}"#;
pub(crate) const RESPONSES_OK: &str =
    r#"{"output":[{"type":"message","content":[{"type":"output_text","text":"ok"}]}]}"#;
pub(crate) const ANTHROPIC_OK: &str = r#"{"id":"msg_fixture","type":"message","role":"assistant","model":"fixture","content":[{"type":"text","text":"ok"}],"stop_reason":"end_turn","usage":{"input_tokens":1,"output_tokens":1}}"#;
pub(crate) const GEMINI_OK: &str =
    r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ok"}]}}]}"#;
pub(crate) const BEDROCK_OK: &str = r#"{"output":{"message":{"role":"assistant","content":[{"text":"ok"}]}},"stopReason":"end_turn"}"#;
pub(crate) const HAILO_OK: &str = r#"{"model":"qwen3:1.7b","message":{"role":"assistant","content":"ok"},"done":true,"prompt_eval_count":1,"eval_count":1}"#;

pub(crate) async fn assert_two_requests(provider: &dyn ModelProvider, server: &Server) {
    let messages = [ChatMessage::user("fixture")];
    let request = || ChatRequest {
        messages: &messages,
        tools: None,
        thinking: None,
    };
    let budget = GenerationAttemptBudget::new(2);
    at_endpoint(&server.url, async {
        for _ in 0..2 {
            let response = provider
                .chat_with_attempt_budget(
                    request(),
                    "fixture",
                    None,
                    ToolRoundPolicy::Single,
                    &budget,
                )
                .await
                .unwrap();
            assert_eq!(response.text.as_deref(), Some("ok"));
        }
        let error = provider
            .chat_with_attempt_budget(request(), "fixture", None, ToolRoundPolicy::Single, &budget)
            .await
            .unwrap_err();
        assert!(error.is::<GenerationAttemptBudgetExhausted>(), "{error:#}");
    })
    .await;
    assert_eq!(server.requests.lock().len(), 2);
    assert_eq!(budget.remaining(), 0);
}

pub(crate) async fn call(
    provider: &dyn ModelProvider,
    budget: &GenerationAttemptBudget,
    tools: Option<&[zeroclaw_api::tool::ToolSpec]>,
) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
    let messages = [ChatMessage::user("fixture")];
    provider
        .chat_with_attempt_budget(
            ChatRequest {
                messages: &messages,
                tools,
                thinking: None,
            },
            "gpt-5",
            None,
            ToolRoundPolicy::Single,
            budget,
        )
        .await
}

fn leaf(marker: &str) -> Box<dyn ModelProvider> {
    Box::new(
        crate::compatible::OpenAiCompatibleModelProvider::builder(marker)
            .display_name("fixture")
            .auth_style(crate::compatible::AuthStyle::Bearer)
            .base_url("http://127.0.0.1:9")
            .credential(Some("fixture-key"))
            .extra_body(serde_json::json!({"fixture_route": marker}))
            .build(),
    )
}

#[tokio::test]
async fn retries_share_budget_and_do_not_reach_a_third_candidate_or_backoff() {
    let server = server(vec![
        Reply::json(500, "first failure"),
        Reply::json(500, "last failure"),
        Reply::json(200, CHAT_OK),
    ])
    .await;
    let provider = crate::reliable::ReliableModelProvider::new(
        "fixture",
        vec![
            ("primary".into(), leaf("primary")),
            ("fallback".into(), leaf("fallback")),
        ],
        2,
        0,
    );
    let budget = GenerationAttemptBudget::new(2);
    let error = at_endpoint(&server.url, call(&provider, &budget, None))
        .await
        .unwrap_err();
    assert!(error.is::<GenerationAttemptBudgetExhausted>(), "{error:#}");
    assert!(format!("{error:#}").contains("last failure"));
    let requests = server.requests.lock();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|(_, body)| body["fixture_route"] == "primary")
    );
}

#[tokio::test]
async fn failover_uses_the_remaining_slot() {
    let server = server(vec![
        Reply::json(500, "first failure"),
        Reply::json(200, CHAT_OK),
    ])
    .await;
    let provider = crate::reliable::ReliableModelProvider::new(
        "fixture",
        vec![
            ("primary".into(), leaf("primary")),
            ("fallback".into(), leaf("fallback")),
        ],
        0,
        0,
    );
    let budget = GenerationAttemptBudget::new(2);
    let response = at_endpoint(&server.url, call(&provider, &budget, None))
        .await
        .unwrap();
    assert_eq!(response.text.as_deref(), Some("ok"));
    let requests = server.requests.lock();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].1["fixture_route"], "fallback");
    assert_eq!(budget.remaining(), 0);
}

#[tokio::test]
async fn exhausted_retry_does_not_sleep_before_returning() {
    let server = server(vec![Reply::json(500, "last failure")]).await;
    let provider = crate::reliable::ReliableModelProvider::new(
        "fixture",
        vec![("primary".into(), leaf("primary"))],
        2,
        10_000,
    );
    let budget = GenerationAttemptBudget::new(1);
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        at_endpoint(&server.url, call(&provider, &budget, None)),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert!(error.is::<GenerationAttemptBudgetExhausted>());
    assert_eq!(server.requests.lock().len(), 1);
}

#[tokio::test]
async fn exhausted_context_recovery_retains_the_latest_cause() {
    let server = server(vec![Reply::json(400, r#"{"error":{"code":"context_length_exceeded","message":"maximum context length exceeded"}}"#)]).await;
    let provider = crate::reliable::ReliableModelProvider::new(
        "fixture",
        vec![("primary".into(), leaf("primary"))],
        2,
        0,
    );
    let budget = GenerationAttemptBudget::new(1);
    let messages = [
        ChatMessage::system("fixture"),
        ChatMessage::user("older"),
        ChatMessage::assistant("older answer"),
        ChatMessage::user("current"),
    ];
    let error = at_endpoint(
        &server.url,
        provider.chat_with_attempt_budget(
            ChatRequest {
                messages: &messages,
                tools: None,
                thinking: None,
            },
            "fixture",
            None,
            ToolRoundPolicy::Single,
            &budget,
        ),
    )
    .await
    .unwrap_err();
    assert!(error.is::<GenerationAttemptBudgetExhausted>(), "{error:#}");
    assert!(
        format!("{error:#}").contains("context_length_exceeded"),
        "{error:#}"
    );
    assert_eq!(server.requests.lock().len(), 1);
}

struct Unsupported;
impl zeroclaw_api::attribution::Attributable for Unsupported {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        use zeroclaw_api::attribution::*;
        Role::Provider(ProviderKind::Model(ModelProviderKind::OpenAi))
    }
    fn alias(&self) -> &str {
        "unsupported"
    }
}
#[async_trait::async_trait]
impl ModelProvider for Unsupported {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        panic!("bounded dispatch must not call the ordinary provider entry")
    }
}

struct EmptyCompletion {
    error: bool,
}
impl zeroclaw_api::attribution::Attributable for EmptyCompletion {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Attributable::role(&Unsupported)
    }
    fn alias(&self) -> &str {
        "empty-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for EmptyCompletion {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        panic!("unexpected ordinary call")
    }
    async fn chat_with_attempt_budget(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
        _: ToolRoundPolicy,
        budget: &GenerationAttemptBudget,
    ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
        budget.try_spend()?;
        if self.error {
            return Err(zeroclaw_api::model_provider::SemanticEmptyTerminalCompletion.into());
        }
        Ok(zeroclaw_api::model_provider::ChatResponse {
            text: None,
            tool_calls: Vec::new(),
            usage: None,
            reasoning_content: None,
        })
    }
}

#[tokio::test(start_paused = true)]
async fn peer_exhaustion_during_empty_backoff_retains_the_empty_cause() {
    for error in [false, true] {
        let provider = crate::reliable::ReliableModelProvider::new(
            "fixture",
            vec![("empty".into(), Box::new(EmptyCompletion { error }))],
            1,
            10_000,
        );
        let budget = GenerationAttemptBudget::new(2);
        let child_budget = budget.clone();
        let task =
            ::zeroclaw_spawn::spawn!(async move { call(&provider, &child_budget, None).await });
        while budget.remaining() == 2 {
            tokio::task::yield_now().await;
        }
        budget.try_spend().unwrap();
        tokio::time::advance(std::time::Duration::from_secs(20)).await;
        let error = task.await.unwrap().unwrap_err();
        assert!(error.is::<GenerationAttemptBudgetExhausted>());
        assert!(
            error
                .chain()
                .any(|source| source.is::<crate::reliable::ReliableSemanticEmptyCompletion>()),
            "{error:#}"
        );
    }
}

#[tokio::test]
async fn unsupported_default_sends_nothing_and_failover_can_continue() {
    let budget = GenerationAttemptBudget::new(2);
    assert!(
        call(&Unsupported, &budget, None)
            .await
            .unwrap_err()
            .is::<zeroclaw_api::model_provider::BoundedChatUnsupported>()
    );
    assert_eq!(budget.remaining(), 2);
    let server = server(vec![Reply::json(200, CHAT_OK)]).await;
    let provider = crate::reliable::ReliableModelProvider::new(
        "fixture",
        vec![
            ("unsupported".into(), Box::new(Unsupported)),
            ("http".into(), leaf("http")),
        ],
        2,
        0,
    );
    at_endpoint(&server.url, call(&provider, &budget, None))
        .await
        .unwrap();
    assert_eq!(server.requests.lock().len(), 1);
    assert_eq!(budget.remaining(), 1);
}

#[tokio::test]
async fn wrappers_forward_one_budget_and_the_pinned_model() {
    let server = server(vec![Reply::json(200, CHAT_OK), Reply::json(200, CHAT_OK)]).await;
    let pinned = crate::model_pin::ModelPinnedProvider::builder("pin")
        .pinned_model("pinned")
        .inner(leaf("http"))
        .build();
    let decorated = crate::vision_override::VisionOverrideProvider::new(Box::new(pinned), false);
    let router = crate::router::RouterModelProvider::new(
        "router",
        vec![("http".into(), Box::new(decorated))],
        vec![],
        "default".into(),
    );
    let provider: Arc<dyn ModelProvider> = Arc::new(router);
    let budget = GenerationAttemptBudget::new(2);
    let messages = [ChatMessage::user("fixture")];
    at_endpoint(&server.url, async {
        // The owned and borrowed dispatch entry points both retain the handle.
        let dispatch = crate::ProviderDispatch::new(provider.clone());
        dispatch
            .chat_with_attempt_budget(
                ChatRequest {
                    messages: &messages,
                    tools: None,
                    thinking: None,
                },
                "requested",
                None,
                ToolRoundPolicy::Single,
                &budget,
            )
            .await
            .unwrap();
        call(&provider, &budget, None).await.unwrap();
        let error = call(&provider, &budget, None).await.unwrap_err();
        assert!(error.is::<GenerationAttemptBudgetExhausted>());
    })
    .await;
    assert_eq!(server.requests.lock().len(), 2);
    assert!(
        server
            .requests
            .lock()
            .iter()
            .all(|(_, body)| body["model"] == "pinned")
    );
}

#[tokio::test]
async fn configured_ollama_and_responses_variants_opt_in() {
    use crate::factory::FamilyProviderFactory;
    use zeroclaw_config::schema::{CustomModelProviderConfig, OllamaModelProviderConfig, WireApi};
    let server = server(vec![Reply::json(200, CHAT_OK), Reply::json(200, CHAT_OK)]).await;
    let provider = OllamaModelProviderConfig::default()
        .create_provider("fixture", None, Some(&server.url), &Default::default())
        .unwrap();
    assert_two_requests(&*provider, &server).await;
    assert!(
        server.requests.lock()[0]
            .0
            .ends_with("/v1/chat/completions")
    );
    let server = self::server(vec![
        Reply::json(200, RESPONSES_OK),
        Reply::json(200, RESPONSES_OK),
    ])
    .await;
    let mut config = CustomModelProviderConfig::default();
    config.base.wire_api = Some(WireApi::Responses);
    let provider = config
        .create_provider(
            "fixture",
            Some("fixture-key"),
            Some(&server.url),
            &Default::default(),
        )
        .unwrap();
    assert_two_requests(&*provider, &server).await;
    assert!(server.requests.lock()[0].0.ends_with("/responses"));
}

#[tokio::test]
async fn cli_and_direct_native_ollama_decline_without_spending() {
    let cwd = tempfile::tempdir().unwrap();
    let executable = std::env::current_exe().unwrap();
    let providers: Vec<Box<dyn ModelProvider>> = vec![
        Box::new(
            crate::gemini_cli::GeminiCliModelProvider::builder("fixture")
                .binary_path(Some("must-not-be-launched"))
                .build(),
        ),
        Box::new(
            crate::kilocli::KiloCliModelProvider::builder("fixture")
                .binary_path(Some("must-not-be-launched"))
                .build(),
        ),
        Box::new(
            crate::grok_cli::GrokCliModelProvider::builder("fixture")
                .binary_path(Some(executable.to_str().unwrap()))
                .working_directory(cwd.path().to_str().unwrap())
                .build()
                .unwrap(),
        ),
        Box::new(crate::ollama::OllamaModelProvider::builder("fixture").build()),
    ];
    for provider in providers {
        let budget = GenerationAttemptBudget::new(2);
        let error = call(&*provider, &budget, None).await.unwrap_err();
        assert!(
            error.is::<zeroclaw_api::model_provider::BoundedChatUnsupported>(),
            "{error:#}"
        );
        assert_eq!(budget.remaining(), 2);
    }
}

#[derive(Debug, thiserror::Error)]
#[error("wrapped bounded provider failure")]
struct WrappedFailure(#[source] Box<dyn std::error::Error + Send + Sync>);

struct WrappedRejection {
    terminal: bool,
    calls: Arc<std::sync::atomic::AtomicUsize>,
}
impl zeroclaw_api::attribution::Attributable for WrappedRejection {
    fn role(&self) -> zeroclaw_api::attribution::Role {
        zeroclaw_api::attribution::Attributable::role(&Unsupported)
    }
    fn alias(&self) -> &str {
        "wrapped-fixture"
    }
}
#[async_trait::async_trait]
impl ModelProvider for WrappedRejection {
    async fn chat_with_system(
        &self,
        _: Option<&str>,
        _: &str,
        _: &str,
        _: Option<f64>,
    ) -> anyhow::Result<String> {
        panic!("unexpected ordinary call")
    }
    async fn chat_with_attempt_budget(
        &self,
        _: ChatRequest<'_>,
        _: &str,
        _: Option<f64>,
        _: ToolRoundPolicy,
        _: &GenerationAttemptBudget,
    ) -> anyhow::Result<zeroclaw_api::model_provider::ChatResponse> {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let error = if self.terminal {
            super::exhausted(anyhow::Error::msg("last provider failure"))
        } else {
            zeroclaw_api::model_provider::BoundedChatUnsupported.into()
        };
        Err(WrappedFailure(error.context("inner annotation").into()).into())
    }
}

#[tokio::test]
async fn reliable_recognizes_control_errors_inside_standard_error_sources() {
    for terminal in [false, true] {
        let server = server(vec![Reply::json(200, CHAT_OK)]).await;
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let provider = crate::reliable::ReliableModelProvider::new(
            "fixture",
            vec![
                (
                    "wrapped".into(),
                    Box::new(WrappedRejection {
                        terminal,
                        calls: calls.clone(),
                    }),
                ),
                ("http".into(), leaf("http")),
            ],
            2,
            0,
        );
        let budget = GenerationAttemptBudget::new(2);
        let result = at_endpoint(&server.url, call(&provider, &budget, None)).await;
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
        if terminal {
            let error = result.unwrap_err();
            assert!(error.is::<GenerationAttemptBudgetExhausted>());
            assert!(format!("{error:#}").contains("last provider failure"));
            assert!(server.requests.lock().is_empty());
            assert_eq!(budget.remaining(), 2);
        } else {
            result.unwrap();
            assert_eq!(server.requests.lock().len(), 1);
            assert_eq!(budget.remaining(), 1);
        }
    }
}

#[tokio::test]
async fn unpolled_send_spends_nothing_and_failed_send_is_not_refunded() {
    let budget = GenerationAttemptBudget::new(2);
    let client = super::client(reqwest::Client::builder().no_proxy(), None).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    drop(listener);
    let future = super::send(client.post(&endpoint), Some(&budget), None);
    drop(future);
    assert_eq!(budget.remaining(), 2);
    assert!(
        super::send(client.post(&endpoint), Some(&budget), None)
            .await
            .is_err()
    );
    assert_eq!(budget.remaining(), 1);
    assert!(
        super::send(client.post("invalid URL"), Some(&budget), None)
            .await
            .is_err()
    );
    assert_eq!(budget.remaining(), 1);
}

#[test]
fn bounded_client_build_failure_does_not_restore_a_default_client() {
    assert!(
        super::client(
            reqwest::Client::builder().user_agent("invalid\nheader"),
            None
        )
        .is_err()
    );
}

#[tokio::test]
async fn bounded_transport_does_not_follow_generation_redirects() {
    use axum::routing::post;
    let arrived = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = arrived.clone();
    let app = Router::new()
        .route(
            "/first",
            post(|| async { (StatusCode::TEMPORARY_REDIRECT, [("location", "/second")]) }),
        )
        .route(
            "/second",
            post(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                async { "unexpected" }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/first", listener.local_addr().unwrap());
    let task = ::zeroclaw_spawn::spawn!(async move { axum::serve(listener, app).await.unwrap() });
    let budget = GenerationAttemptBudget::new(2);
    let client = super::client(reqwest::Client::builder().no_proxy(), None).unwrap();
    let response = super::send(client.post(endpoint).body("fixture"), Some(&budget), None)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(arrived.load(std::sync::atomic::Ordering::Relaxed), 0);
    assert_eq!(budget.remaining(), 1);
    task.abort();
}
