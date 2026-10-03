use super::*;
use std::net::SocketAddr;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderName, HeaderValue, Method as HttpMethod, Request, StatusCode, header};
use axum::response::Response;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use tower::ServiceExt;
use zeroclaw_api::jsonrpc::error_codes::INVALID_PARAMS;
use zeroclaw_api::webhook::{
    MAX_PLUGIN_WEBHOOK_HEADERS, MAX_WEBHOOK_RESPONSE_BODY_BYTES, PluginWebhookOutcome,
};
use zeroclaw_rpc_client::ConnectOptions;
use zeroclaw_rpc_proto::types::is_valid_plugin_webhook_request_id;

use crate::MAX_BODY_SIZE;
use crate::core_rpc::test_support::{FakeCore, serve_fake_core};
use crate::plugin_webhook::PluginWebhookBackend;
use crate::plugin_webhook::tests::{backend_test_router, plugin_webhook_request, response_text};
use crate::tests::admin_paircode_state;

const DISPATCH: &str = "plugin-webhook/dispatch";
const CANCEL: &str = "plugin-webhook/cancel";
const TEXT: &str = "text/plain; charset=utf-8";

fn capabilities() -> [&'static str; 3] {
    [
        Method::PluginWebhookDispatch.wire_name(),
        Method::PluginWebhookCancel.wire_name(),
        Method::PluginWebhookRoutes.wire_name(),
    ]
}

/// A core answer that serves `plugin-webhook/cancel` and answers dispatches
/// with `dispatch(params)`.
fn core_answer(
    dispatch: impl Fn(&Value) -> Option<Result<Value, i32>> + Send + 'static,
) -> impl Fn(&str, &Value) -> Option<Result<Value, i32>> + Send + 'static {
    move |method, params| match method {
        DISPATCH => dispatch(params),
        CANCEL => Some(Ok(json!({"cancelled": true}))),
        _ => Some(Err(METHOD_NOT_FOUND)),
    }
}

/// A dispatch answer taken from the request body, so one core can answer
/// each test request differently.
fn answer_from_body(params: &Value) -> Option<Result<Value, i32>> {
    let body = decoded_body(params);
    Some(Ok(
        serde_json::from_slice(&body).expect("the test body is JSON")
    ))
}

fn decoded_body(params: &Value) -> Vec<u8> {
    STANDARD
        .decode(params["body_b64"].as_str().expect("body_b64 is a string"))
        .expect("body_b64 is standard base64")
}

/// A forwarder connected to a fake core over an in-memory stream.
struct Forwarding {
    core: CoreRpc,
    fake: FakeCore,
    _state_dir: tempfile::TempDir,
    router: Router,
}

async fn forwarding(
    capabilities: &[&str],
    answer: impl Fn(&str, &Value) -> Option<Result<Value, i32>> + Send + 'static,
) -> Forwarding {
    let (client_half, core_half) = tokio::io::duplex(1024 * 1024);
    let fake = serve_fake_core(core_half, capabilities, None, answer);
    let client = RpcClient::connect_over(client_half, ConnectOptions::default())
        .await
        .expect("the fake core completes the handshake");
    let core = CoreRpc::connected_for_test(client).await;
    router_over(core, fake)
}

fn router_over(core: CoreRpc, fake: FakeCore) -> Forwarding {
    let state_dir = tempfile::TempDir::new().expect("temp dir");
    let router = backend_test_router(
        admin_paircode_state(&state_dir, false, false),
        PluginWebhookBackend::Core(core.clone()),
    );
    Forwarding {
        core,
        fake,
        _state_dir: state_dir,
        router,
    }
}

/// A router over `core` with no fake core behind it.
fn router_without_core(core: CoreRpc) -> (Router, tempfile::TempDir) {
    let state_dir = tempfile::TempDir::new().expect("temp dir");
    let router = backend_test_router(
        admin_paircode_state(&state_dir, false, false),
        PluginWebhookBackend::Core(core),
    );
    (router, state_dir)
}

fn peer() -> SocketAddr {
    SocketAddr::from(([127, 0, 0, 1], 31_000))
}

fn post(body: impl Into<Body>) -> Request<Body> {
    plugin_webhook_request("fixture", body, peer(), None)
}

async fn send(router: &Router, request: Request<Body>) -> Response {
    router
        .clone()
        .oneshot(request)
        .await
        .expect("plugin route is infallible")
}

/// The next frame of `method` the core received, skipping the others.
async fn next_call(fake: &mut FakeCore, method: &str) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = fake
                .frames
                .recv()
                .await
                .expect("the core connection is open");
            if frame["method"] == method {
                return frame;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the core never received {method}"))
}

/// The frames of `method` the core has received so far.
fn received(fake: &mut FakeCore, method: &str) -> Vec<Value> {
    let mut calls = Vec::new();
    while let Ok(frame) = fake.frames.try_recv() {
        if frame["method"] == method {
            calls.push(frame);
        }
    }
    calls
}

async fn expect_response(response: Response, status: StatusCode, body: &str) {
    assert_eq!(response.status(), status);
    assert_eq!(response_text(response).await, body);
}

#[tokio::test]
async fn forwarded_request_carries_the_exact_method_query_headers_and_body() {
    let mut forwarding = forwarding(
        &capabilities(),
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;
    let body = vec![0xff, 0x00, b'x'];
    let mut request = post(body.clone());
    *request.uri_mut() = "/plugin/fixture?a=1&b=2".parse().expect("valid URI");
    let headers = request.headers_mut();
    headers.insert("X-Custom", HeaderValue::from_static("v"));
    headers.append("x-dup", HeaderValue::from_static("one"));
    headers.append("x-dup", HeaderValue::from_static("two"));
    headers.insert(
        HeaderName::from_static("x-binary"),
        HeaderValue::from_bytes(&[0xff, 0xfe]).expect("obs-text is a valid header value"),
    );

    let response = send(&forwarding.router, request).await;
    expect_response(response, StatusCode::OK, "").await;
    let frame = next_call(&mut forwarding.fake, DISPATCH).await;
    let params = &frame["params"];
    assert_eq!(params["method"], "POST");
    assert_eq!(params["path"], "fixture");
    assert_eq!(params["query"], "a=1&b=2");
    // A value outside visible ASCII is dropped, as it is in process; the
    // rest keep their order and repeats.
    assert_eq!(
        params["headers"],
        json!([
            {"name": "x-custom", "value": "v"},
            {"name": "x-dup", "value": "one"},
            {"name": "x-dup", "value": "two"},
        ])
    );
    assert_eq!(decoded_body(params), body);
    let request_id = params["request_id"]
        .as_str()
        .expect("request_id is a string");
    assert!(
        is_valid_plugin_webhook_request_id(request_id),
        "{request_id:?}"
    );
}

#[tokio::test]
async fn every_core_outcome_maps_to_the_contract_response() {
    let forwarding = forwarding(&capabilities(), core_answer(answer_from_body)).await;
    let oversized = "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES + 1);
    let largest = "x".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES);
    for (result, status, content_type, body) in [
        (json!({"outcome": "ack"}), StatusCode::OK, None, ""),
        (
            json!({"outcome": "reply", "body": "hi"}),
            StatusCode::OK,
            Some(TEXT),
            "hi",
        ),
        (
            json!({"outcome": "reply", "body": largest}),
            StatusCode::OK,
            Some(TEXT),
            largest.as_str(),
        ),
        (
            json!({"outcome": "reply", "body": oversized}),
            StatusCode::BAD_GATEWAY,
            Some(TEXT),
            "invalid webhook response",
        ),
        (
            json!({"outcome": "not_found"}),
            StatusCode::NOT_FOUND,
            Some(TEXT),
            "webhook not found",
        ),
        (
            json!({"outcome": "queue_full"}),
            StatusCode::TOO_MANY_REQUESTS,
            Some(TEXT),
            "webhook queue full",
        ),
        (
            json!({"outcome": "unavailable"}),
            StatusCode::SERVICE_UNAVAILABLE,
            Some(TEXT),
            "webhook unavailable",
        ),
        (
            json!({"outcome": "unauthorized"}),
            StatusCode::UNAUTHORIZED,
            Some(TEXT),
            "unauthorized webhook",
        ),
        (
            json!({"outcome": "bad_request"}),
            StatusCode::BAD_REQUEST,
            Some(TEXT),
            "invalid webhook",
        ),
        (
            json!({"outcome": "invalid_response"}),
            StatusCode::BAD_GATEWAY,
            Some(TEXT),
            "invalid webhook response",
        ),
        (
            json!({"outcome": "timeout"}),
            StatusCode::GATEWAY_TIMEOUT,
            Some(TEXT),
            "webhook processing timed out",
        ),
        (
            json!({"outcome": "cancelled"}),
            StatusCode::SERVICE_UNAVAILABLE,
            Some(TEXT),
            "webhook unavailable",
        ),
    ] {
        let shown = result["outcome"].to_string();
        let response = send(&forwarding.router, post(result.to_string())).await;
        assert_eq!(response.status(), status, "{shown}");
        assert_eq!(
            response
                .headers()
                .get(header::CONTENT_TYPE)
                .map(|value| value.to_str().expect("ASCII content type")),
            content_type,
            "{shown}"
        );
        assert_eq!(response_text(response).await, body, "{shown}");
    }
}

#[tokio::test]
async fn a_core_without_dispatch_support_gets_503_and_no_call() {
    let mut forwarding = forwarding(
        &[Method::Status.wire_name()],
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;
    let request = PluginWebhookRequest::new("fixture", "POST", "", Vec::new(), b"{}".to_vec())
        .expect("a valid request");
    assert_eq!(
        dispatch(&forwarding.core, request).await,
        Dispatched::CoreUnsupported
    );
    let response = send(&forwarding.router, post("{}")).await;
    expect_response(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "webhook unavailable",
    )
    .await;
    assert!(received(&mut forwarding.fake, DISPATCH).is_empty());
}

#[tokio::test]
async fn no_core_connection_fails_fast_with_503() {
    let (router, _state_dir) = router_without_core(CoreRpc::default());
    let started = tokio::time::Instant::now();
    let response = send(&router, post("{}")).await;
    expect_response(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "webhook unavailable",
    )
    .await;
    assert!(started.elapsed() < Duration::from_secs(1));
}

#[tokio::test]
async fn a_refused_core_link_returns_503() {
    let core = CoreRpc::refused_for_test(AUTH_REQUIRED).await;
    let request = PluginWebhookRequest::new("fixture", "GET", "", Vec::new(), Vec::new())
        .expect("a valid request");
    assert_eq!(
        dispatch(&core, request).await,
        Dispatched::CoreRefused {
            code: AUTH_REQUIRED
        }
    );
    let (router, _state_dir) = router_without_core(core);
    let response = send(&router, post("{}")).await;
    expect_response(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "webhook unavailable",
    )
    .await;
}

#[tokio::test]
async fn a_client_whose_connection_already_ended_fails_fast_with_503() {
    let mut forwarding = forwarding(
        &capabilities(),
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;
    let client = forwarding
        .core
        .client()
        .await
        .expect("the forwarder is connected");
    forwarding.fake.close.cancel();
    tokio::time::timeout(Duration::from_secs(2), client.closed())
        .await
        .expect("the client observes the close");
    assert!(
        forwarding.core.is_connected().await,
        "nothing has replaced the link yet"
    );
    let response = send(&forwarding.router, post("{}")).await;
    expect_response(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "webhook unavailable",
    )
    .await;
    assert!(received(&mut forwarding.fake, DISPATCH).is_empty());
}

#[tokio::test]
async fn core_rpc_errors_map_to_503() {
    let forwarding = forwarding(
        &capabilities(),
        core_answer(|params| {
            let code = String::from_utf8(decoded_body(params)).expect("UTF-8 test body");
            Some(Err(code.parse().expect("the test body is an error code")))
        }),
    )
    .await;
    for (code, expected) in [
        (FORBIDDEN, Dispatched::CoreRefused { code: FORBIDDEN }),
        (
            AUTH_REQUIRED,
            Dispatched::CoreRefused {
                code: AUTH_REQUIRED,
            },
        ),
        (METHOD_NOT_FOUND, Dispatched::CoreUnsupported),
        (
            INVALID_PARAMS,
            Dispatched::CoreUnexpected {
                reason: "core returned an error",
                code: Some(INVALID_PARAMS),
            },
        ),
    ] {
        let request =
            PluginWebhookRequest::new("fixture", "POST", "", Vec::new(), code.to_string().into())
                .expect("a valid request");
        assert_eq!(
            dispatch(&forwarding.core, request).await,
            expected,
            "{code}"
        );
        let response = send(&forwarding.router, post(code.to_string())).await;
        expect_response(
            response,
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook unavailable",
        )
        .await;
    }
}

#[tokio::test]
async fn unusable_core_results_are_503() {
    let forwarding = forwarding(&capabilities(), core_answer(answer_from_body)).await;
    for (result, reason) in [
        (json!({"outcome": "bogus"}), "undecodable dispatch result"),
        (json!({"outcome": "reply"}), "reply_without_body"),
        (json!({"outcome": "ack", "body": "x"}), "body_without_reply"),
        (json!("ack"), "undecodable dispatch result"),
    ] {
        let request = PluginWebhookRequest::new(
            "fixture",
            "POST",
            "",
            Vec::new(),
            result.to_string().into_bytes(),
        )
        .expect("a valid request");
        assert_eq!(
            dispatch(&forwarding.core, request).await,
            Dispatched::CoreUnexpected { reason, code: None },
            "{result}"
        );
        let response = send(&forwarding.router, post(result.to_string())).await;
        expect_response(
            response,
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook unavailable",
        )
        .await;
    }
}

#[tokio::test]
async fn the_core_dropping_mid_request_returns_503_promptly() {
    let mut forwarding = forwarding(&capabilities(), core_answer(|_| None)).await;
    let router = forwarding.router.clone();
    let pending = zeroclaw_spawn::spawn!(async move { send(&router, post("{}")).await });
    next_call(&mut forwarding.fake, DISPATCH).await;
    let closed_at = tokio::time::Instant::now();
    forwarding.fake.close.cancel();
    let response = tokio::time::timeout(Duration::from_secs(2), pending)
        .await
        .expect("the forwarder answers once the core is gone")
        .expect("the request task joins");
    expect_response(
        response,
        StatusCode::SERVICE_UNAVAILABLE,
        "webhook unavailable",
    )
    .await;
    assert!(closed_at.elapsed() < Duration::from_secs(2));
}

#[tokio::test(start_paused = true)]
async fn a_core_that_never_answers_times_out_with_504_and_is_cancelled() {
    let mut forwarding = forwarding(&capabilities(), core_answer(|_| None)).await;
    let started = tokio::time::Instant::now();
    let response = send(&forwarding.router, post("{}")).await;
    expect_response(
        response,
        StatusCode::GATEWAY_TIMEOUT,
        "webhook processing timed out",
    )
    .await;
    assert!(started.elapsed() >= PLUGIN_WEBHOOK_DEADLINE + CORE_TIMEOUT_MARGIN);
    let dispatched = next_call(&mut forwarding.fake, DISPATCH).await;
    let cancelled = next_call(&mut forwarding.fake, CANCEL).await;
    assert_eq!(
        cancelled["params"]["request_id"],
        dispatched["params"]["request_id"]
    );
}

#[tokio::test]
async fn dropping_the_request_future_sends_a_best_effort_cancel() {
    let mut forwarding = forwarding(&capabilities(), core_answer(|_| None)).await;
    let abandoned = tokio::time::timeout(
        Duration::from_millis(200),
        send(&forwarding.router, post("{}")),
    )
    .await;
    assert!(abandoned.is_err(), "the core never answers");
    let dispatched = next_call(&mut forwarding.fake, DISPATCH).await;
    let cancelled = next_call(&mut forwarding.fake, CANCEL).await;
    assert_eq!(
        cancelled["params"]["request_id"],
        dispatched["params"]["request_id"]
    );
}

#[tokio::test]
async fn a_completed_dispatch_sends_no_cancel() {
    let mut forwarding = forwarding(
        &capabilities(),
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;
    let response = send(&forwarding.router, post("{}")).await;
    expect_response(response, StatusCode::OK, "").await;
    next_call(&mut forwarding.fake, DISPATCH).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(received(&mut forwarding.fake, CANCEL).is_empty());
}

#[tokio::test]
async fn request_ids_are_unique_per_dispatch() {
    let mut forwarding = forwarding(
        &capabilities(),
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;
    for _ in 0..2 {
        let response = send(&forwarding.router, post("{}")).await;
        expect_response(response, StatusCode::OK, "").await;
    }
    let first = next_call(&mut forwarding.fake, DISPATCH).await;
    let second = next_call(&mut forwarding.fake, DISPATCH).await;
    assert_ne!(
        first["params"]["request_id"],
        second["params"]["request_id"]
    );
}

#[tokio::test]
async fn admission_failures_never_reach_the_core() {
    let mut forwarding = forwarding(
        &capabilities(),
        core_answer(|_| Some(Ok(json!({"outcome": "ack"})))),
    )
    .await;

    let mut head = post("");
    *head.method_mut() = HttpMethod::HEAD;
    let response = send(&forwarding.router, head).await;
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()[header::ALLOW], "GET, POST");

    let response = send(&forwarding.router, post(vec![b'x'; MAX_BODY_SIZE + 1])).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);

    let mut crowded = post("{}");
    for index in 0..=MAX_PLUGIN_WEBHOOK_HEADERS {
        crowded.headers_mut().insert(
            HeaderName::try_from(format!("x-h-{index}")).expect("valid header name"),
            HeaderValue::from_static("v"),
        );
    }
    let response = send(&forwarding.router, crowded).await;
    expect_response(response, StatusCode::BAD_REQUEST, "invalid webhook").await;

    assert!(received(&mut forwarding.fake, DISPATCH).is_empty());
}

#[tokio::test]
async fn an_ingress_outcome_and_its_forwarded_twin_share_one_response() {
    let forwarding = forwarding(&capabilities(), core_answer(answer_from_body)).await;
    for outcome in [
        PluginWebhookOutcome::Ack,
        PluginWebhookOutcome::Reply("challenge".into()),
        PluginWebhookOutcome::NotFound,
        PluginWebhookOutcome::Timeout,
    ] {
        let wire = serde_json::to_string(&PluginWebhookDispatchResult::from(outcome.clone()))
            .expect("results encode");
        let forwarded = send(&forwarding.router, post(wire)).await;
        let in_process = super::super::outcome_response(outcome.clone());
        assert_eq!(forwarded.status(), in_process.status(), "{outcome:?}");
        assert_eq!(
            response_text(forwarded).await,
            response_text(in_process).await,
            "{outcome:?}"
        );
    }
}
