use super::*;
use std::collections::HashMap;
use std::time::Duration;

use axum::{
    body::Body,
    http::{HeaderValue, Request},
};
use http_body_util::BodyExt;
use tower::ServiceExt;
use tower_http::limit::RequestBodyLimitLayer;

use crate::{
    GatewayRateLimiter, MAX_BODY_SIZE, SlidingWindowRateLimiter, tests::admin_paircode_state,
};
use zeroclaw_api::webhook::{PluginWebhookOwner, PluginWebhookRoute, RawWebhook, WebhookOutcome};

fn test_ingress() -> Arc<PluginWebhookIngress> {
    Arc::new(PluginWebhookIngress::new(300, 8))
}

fn fixture_route(sink: tokio::sync::mpsc::Sender<RawWebhook>) -> PluginWebhookRoute {
    PluginWebhookRoute::new(PluginWebhookOwner::new("fixture-plugin", "fixture"), sink)
}

fn plugin_webhook_test_router(state: AppState, ingress: Arc<PluginWebhookIngress>) -> Router {
    routes(ingress)
        .with_state(state)
        .layer(RequestBodyLimitLayer::new(MAX_BODY_SIZE))
}

fn plugin_webhook_request(
    path: &str,
    body: impl Into<Body>,
    peer: SocketAddr,
    forwarded_for: Option<&str>,
) -> Request<Body> {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/plugin/{path}"));
    if let Some(forwarded_for) = forwarded_for {
        request = request.header("x-forwarded-for", forwarded_for);
    }
    let mut request = request
        .body(body.into())
        .expect("valid plugin webhook request");
    request.extensions_mut().insert(ConnectInfo(peer));
    request
}

async fn response_text(response: Response) -> String {
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("response body collects")
        .to_bytes();
    String::from_utf8(bytes.to_vec()).expect("fixed webhook response is utf-8")
}

#[tokio::test]
async fn plugin_webhook_router_delivers_exact_request() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let ingress = test_ingress();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    let registry_lease = ingress.registry().start_generation();
    assert!(registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let (seen, observed) = tokio::sync::oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards request");
        let _ = seen.send((request.headers.clone(), request.body.clone()));
        let _ = request.reply.send(Ok(WebhookOutcome::Ack));
    });

    let peer = SocketAddr::from(([203, 0, 113, 7], 30_300));
    let mut request = plugin_webhook_request("fixture", "signed body", peer, None);
    request
        .headers_mut()
        .insert("x-fixture-secret", HeaderValue::from_static("test-secret"));
    let response = plugin_webhook_test_router(state, ingress)
        .oneshot(request)
        .await
        .expect("plugin route is infallible");
    assert_eq!(response.status(), StatusCode::OK);
    let (headers, body) = observed.await.expect("sink records request");
    assert_eq!(body, b"signed body");
    assert!(
        headers
            .iter()
            .any(|(name, value)| name == "x-fixture-secret" && value == "test-secret")
    );
}

#[tokio::test]
async fn plugin_webhook_router_keeps_all_diagnostics_private() {
    use zeroclaw_api::webhook::WebhookReject;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let ingress = test_ingress();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(3);
    let registry_lease = ingress.registry().start_generation();
    assert!(registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    zeroclaw_spawn::spawn!(async move {
        while let Some(request) = receiver.recv().await {
            let rejection = match request.body.as_slice() {
                b"auth" => WebhookReject::Unauthorized(
                    "signature mismatch for private operator token".to_string(),
                ),
                b"guest" => {
                    WebhookReject::BadRequest("private payload parser diagnostic".to_string())
                }
                b"response" => WebhookReject::InvalidResponse,
                _ => WebhookReject::Unavailable("wasmtime trap with private host path".to_string()),
            };
            let _ = request.reply.send(Err(rejection));
        }
    });
    let app = plugin_webhook_test_router(state, ingress);
    let peer = SocketAddr::from(([203, 0, 113, 8], 30_301));

    for (body, status, public) in [
        ("auth", StatusCode::UNAUTHORIZED, "unauthorized webhook"),
        ("guest", StatusCode::BAD_REQUEST, "invalid webhook"),
        (
            "response",
            StatusCode::BAD_GATEWAY,
            "invalid webhook response",
        ),
        (
            "host",
            StatusCode::SERVICE_UNAVAILABLE,
            "webhook unavailable",
        ),
    ] {
        let response = app
            .clone()
            .oneshot(plugin_webhook_request("fixture", body, peer, None))
            .await
            .expect("plugin route is infallible");
        assert_eq!(response.status(), status);
        assert_eq!(response_text(response).await, public);
    }
}

#[tokio::test(start_paused = true)]
async fn plugin_webhook_router_timeout_cancels_the_worker_request() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let ingress = test_ingress();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    let registry_lease = ingress.registry().start_generation();
    assert!(registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let (cancelled, observed) = tokio::sync::oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards request");
        request.cancellation.cancelled().await;
        let _ = cancelled.send(());
    });

    let response = plugin_webhook_test_router(state, ingress)
        .oneshot(plugin_webhook_request(
            "fixture",
            "slow",
            SocketAddr::from(([203, 0, 113, 9], 30_302)),
            None,
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert_eq!(
        response_text(response).await,
        "webhook processing timed out"
    );
    observed
        .await
        .expect("handler deadline propagates to the actual worker request");
}

#[tokio::test]
async fn plugin_webhook_router_bounds_queue_body_and_route_availability() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let peer = SocketAddr::from(([203, 0, 113, 10], 30_303));

    let unknown_ingress = test_ingress();
    let unknown = plugin_webhook_test_router(state.clone(), unknown_ingress)
        .oneshot(plugin_webhook_request("missing", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let full_ingress = test_ingress();
    let (full_sink, mut full_receiver) = tokio::sync::mpsc::channel(1);
    let (prefill_reply, _) = tokio::sync::oneshot::channel();
    full_sink
        .try_send(RawWebhook {
            method: "POST".to_string(),
            query: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            cancellation: zeroclaw_api::webhook::WebhookCancellation::new(),
            idempotency: None,
            reply: prefill_reply,
        })
        .expect("prefill bounded queue");
    let full_registry_lease = full_ingress.registry().start_generation();
    assert!(full_registry_lease.replace(HashMap::from([(
        "full".to_string(),
        fixture_route(full_sink)
    )])));
    let full = plugin_webhook_test_router(state.clone(), full_ingress)
        .oneshot(plugin_webhook_request("full", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response_text(full).await, "webhook queue full");
    let _ = full_receiver.recv().await;

    let closed_ingress = test_ingress();
    let (closed_sink, closed_receiver) = tokio::sync::mpsc::channel(1);
    drop(closed_receiver);
    let closed_registry_lease = closed_ingress.registry().start_generation();
    assert!(closed_registry_lease.replace(HashMap::from([(
        "closed".to_string(),
        fixture_route(closed_sink)
    )])));
    let closed = plugin_webhook_test_router(state.clone(), closed_ingress)
        .oneshot(plugin_webhook_request("closed", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(closed.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body_ingress = test_ingress();
    let (body_sink, mut body_receiver) = tokio::sync::mpsc::channel(1);
    let body_registry_lease = body_ingress.registry().start_generation();
    assert!(body_registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(body_sink)
    )])));
    let oversized = plugin_webhook_test_router(state, body_ingress)
        .oneshot(plugin_webhook_request(
            "fixture",
            vec![b'x'; MAX_BODY_SIZE + 1],
            peer,
            None,
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert!(body_receiver.try_recv().is_err());
}

#[tokio::test]
async fn plugin_webhook_router_uses_the_canonical_client_key_policy() {
    async fn acknowledge(mut receiver: tokio::sync::mpsc::Receiver<RawWebhook>) {
        while let Some(request) = receiver.recv().await {
            let _ = request.reply.send(Ok(WebhookOutcome::Ack));
        }
    }

    fn limiter(limit: u32, window: Duration) -> Arc<GatewayRateLimiter> {
        Arc::new(GatewayRateLimiter {
            pair: SlidingWindowRateLimiter::new(100, window, 100),
            webhook: SlidingWindowRateLimiter::new(limit, window, 100),
        })
    }

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let peer = SocketAddr::from(([203, 0, 113, 11], 30_304));
    let mut state = admin_paircode_state(&tmp, false, false);
    state.rate_limiter = limiter(1, Duration::from_millis(25));
    let ingress = test_ingress();
    let (sink, receiver) = tokio::sync::mpsc::channel(4);
    let registry_lease = ingress.registry().start_generation();
    assert!(registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    zeroclaw_spawn::spawn!(acknowledge(receiver));
    let app = plugin_webhook_test_router(state, ingress);

    let first = app
        .clone()
        .oneshot(plugin_webhook_request(
            "fixture",
            "one",
            peer,
            Some("198.51.100.1"),
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(first.status(), StatusCode::OK);
    let second = app
        .clone()
        .oneshot(plugin_webhook_request(
            "fixture",
            "two",
            peer,
            Some("198.51.100.2"),
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    tokio::time::sleep(Duration::from_millis(30)).await;
    let recovered = app
        .oneshot(plugin_webhook_request(
            "fixture",
            "three",
            peer,
            Some("198.51.100.2"),
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(recovered.status(), StatusCode::OK);

    let mut trusted_state = admin_paircode_state(&tmp, false, false);
    trusted_state.trust_forwarded_headers = true;
    trusted_state.rate_limiter = limiter(1, Duration::from_secs(1));
    let trusted_ingress = test_ingress();
    let (trusted_sink, trusted_receiver) = tokio::sync::mpsc::channel(4);
    let trusted_registry_lease = trusted_ingress.registry().start_generation();
    assert!(trusted_registry_lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(trusted_sink)
    )])));
    zeroclaw_spawn::spawn!(acknowledge(trusted_receiver));
    let trusted = plugin_webhook_test_router(trusted_state, trusted_ingress);
    for forwarded in ["198.51.100.1", "198.51.100.2"] {
        let response = trusted
            .clone()
            .oneshot(plugin_webhook_request(
                "fixture",
                "body",
                peer,
                Some(forwarded),
            ))
            .await
            .expect("plugin route is infallible");
        assert_eq!(response.status(), StatusCode::OK);
    }
    let repeated = trusted
        .oneshot(plugin_webhook_request(
            "fixture",
            "body",
            peer,
            Some("198.51.100.1"),
        ))
        .await
        .expect("plugin route is infallible");
    assert_eq!(repeated.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn plugin_webhook_routes_preserve_authoritative_request_metadata() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(2);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress);
    let worker = zeroclaw_spawn::spawn!(async move {
        for method in ["GET", "POST"] {
            let request = receiver
                .recv()
                .await
                .expect("supported method reaches guest");
            assert_eq!(request.method, method);
            assert_eq!(request.query, "challenge=a%2Bb&part=one&part=two");
            assert_eq!(request.body, b"exact bytes");
            assert!(
                request
                    .headers
                    .contains(&("x-webhook-method".to_string(), "DELETE".to_string()))
            );
            request
                .reply
                .send(Ok(WebhookOutcome::Body("echo".to_string())))
                .expect("caller waits");
        }
    });
    for method in [Method::GET, Method::POST] {
        let mut request = plugin_webhook_request(
            "fixture",
            "exact bytes",
            SocketAddr::from(([127, 0, 0, 1], 31000)),
            None,
        );
        *request.method_mut() = method;
        *request.uri_mut() = "/plugin/fixture?challenge=a%2Bb&part=one&part=two"
            .parse()
            .expect("valid URI");
        request
            .headers_mut()
            .insert("X-Webhook-Method", HeaderValue::from_static("DELETE"));
        request
            .headers_mut()
            .insert("X-Webhook-Query", HeaderValue::from_static("spoofed"));
        let response = app.clone().oneshot(request).await.expect("route responds");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response_text(response).await, "echo");
    }
    worker.await.expect("metadata checks passed");
}

#[tokio::test]
async fn plugin_webhook_unsupported_methods_never_reach_the_guest() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress);
    for method in [
        Method::HEAD,
        Method::PUT,
        Method::PATCH,
        Method::DELETE,
        Method::OPTIONS,
        Method::TRACE,
    ] {
        let mut request = plugin_webhook_request(
            "fixture",
            "",
            SocketAddr::from(([127, 0, 0, 1], 31000)),
            None,
        );
        *request.method_mut() = method;
        let response = app.clone().oneshot(request).await.expect("route responds");
        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
        assert_eq!(response.headers()[header::ALLOW], "GET, POST");
        assert!(receiver.try_recv().is_err());
    }
}

#[tokio::test]
async fn plugin_webhook_response_limit_counts_utf8_bytes_at_the_gateway() {
    use zeroclaw_api::webhook::MAX_WEBHOOK_RESPONSE_BODY_BYTES;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress);
    let bodies = [
        String::new(),
        "λ".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES / 2),
        "λ".repeat(MAX_WEBHOOK_RESPONSE_BODY_BYTES / 2 + 1),
    ];
    let responses = bodies.clone();
    let worker = zeroclaw_spawn::spawn!(async move {
        for body in responses {
            let request = receiver.recv().await.expect("request arrives");
            request
                .reply
                .send(Ok(WebhookOutcome::Body(body)))
                .expect("caller waits");
        }
    });
    for body in bodies {
        let response = app
            .clone()
            .oneshot(plugin_webhook_request(
                "fixture",
                "",
                SocketAddr::from(([127, 0, 0, 1], 31000)),
                None,
            ))
            .await
            .expect("route responds");
        if body.len() <= MAX_WEBHOOK_RESPONSE_BODY_BYTES {
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response_text(response).await, body);
        } else {
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(response_text(response).await, "invalid webhook response");
        }
    }
    worker.await.expect("response worker joins");
}

#[tokio::test]
async fn plugin_webhook_request_over_ingress_bounds_is_invalid_before_route_lookup() {
    use zeroclaw_api::webhook::MAX_PLUGIN_WEBHOOK_HEADERS;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress);

    for path in ["fixture", "missing"] {
        let mut request = plugin_webhook_request(
            path,
            "body",
            SocketAddr::from(([127, 0, 0, 1], 31000)),
            None,
        );
        for index in 0..=MAX_PLUGIN_WEBHOOK_HEADERS {
            request.headers_mut().insert(
                axum::http::HeaderName::try_from(format!("x-h-{index}"))
                    .expect("valid header name"),
                HeaderValue::from_static("v"),
            );
        }
        let response = app.clone().oneshot(request).await.expect("route responds");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "path {path}");
        assert_eq!(response_text(response).await, "invalid webhook");
    }
    assert!(receiver.try_recv().is_err());
}

#[tokio::test]
async fn plugin_webhook_dedup_state_outlives_a_gateway_router() {
    use zeroclaw_api::webhook::WebhookReservation;

    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(2);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let (reports, mut reported) = tokio::sync::mpsc::unbounded_channel();
    zeroclaw_spawn::spawn!(async move {
        while let Some(request) = receiver.recv().await {
            let report = match &request.idempotency {
                Some(idempotency) => match idempotency.begin("stable-id") {
                    WebhookReservation::Owner(token) if idempotency.commit(&token) => "owner",
                    WebhookReservation::Committed => "committed",
                    _ => "unexpected reservation",
                },
                None => "no idempotency bridge",
            };
            let _ = reports.send(report);
            let _ = request.reply.send(Ok(WebhookOutcome::Ack));
        }
    });

    // Each router, with its own app state, stands in for one gateway run of
    // the same daemon generation.
    for expected in ["owner", "committed"] {
        let tmp = tempfile::TempDir::new().expect("temp dir");
        let response = plugin_webhook_test_router(
            admin_paircode_state(&tmp, false, false),
            Arc::clone(&ingress),
        )
        .oneshot(plugin_webhook_request(
            "fixture",
            "{}",
            SocketAddr::from(([127, 0, 0, 1], 31000)),
            None,
        ))
        .await
        .expect("route responds");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(reported.recv().await, Some(expected));
    }
}

/// Plugin deliveries and the generic `/webhook` and `/sop/*` routes keep
/// separate committed-key budgets. With room for one key in each store, a
/// plugin commit does not evict a generic key, and generic records do not
/// evict a committed plugin delivery. The stores are the ones a gateway run
/// uses: the core ingress's and the run's `IdempotencyStore`.
#[tokio::test]
async fn plugin_and_generic_webhook_keys_have_separate_budgets() {
    use zeroclaw_api::webhook::WebhookReservation;

    let ingress = Arc::new(PluginWebhookIngress::new(300, 1));
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(2);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let (reports, mut reported) = tokio::sync::mpsc::unbounded_channel();
    zeroclaw_spawn::spawn!(async move {
        while let Some(request) = receiver.recv().await {
            let report = match &request.idempotency {
                Some(idempotency) => match idempotency.begin("stable-id") {
                    WebhookReservation::Owner(token) if idempotency.commit(&token) => "owner",
                    WebhookReservation::Committed => "committed",
                    _ => "unexpected reservation",
                },
                None => "no idempotency bridge",
            };
            let _ = reports.send(report);
            let _ = request.reply.send(Ok(WebhookOutcome::Ack));
        }
    });
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let deliver = |ingress: Arc<PluginWebhookIngress>| {
        plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress).oneshot(
            plugin_webhook_request(
                "fixture",
                "{}",
                SocketAddr::from(([127, 0, 0, 1], 31001)),
                None,
            ),
        )
    };
    let generic = crate::IdempotencyStore::new(Duration::from_secs(300), 1);

    assert!(generic.record_if_new("generic"));
    let response = deliver(Arc::clone(&ingress)).await.expect("route responds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(reported.recv().await, Some("owner"));
    assert!(
        !generic.record_if_new("generic"),
        "a plugin commit must not evict a generic key"
    );

    assert!(generic.record_if_new("generic-2"));
    let response = deliver(Arc::clone(&ingress)).await.expect("route responds");
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        reported.recv().await,
        Some("committed"),
        "generic records must not evict a committed plugin delivery"
    );
}

/// The name the request constructor forwards for `name`, or `None` when it
/// refuses the request.
fn ingress_header_name(name: &str) -> Option<String> {
    PluginWebhookRequest::new(
        "fixture",
        "POST",
        "",
        vec![(name.to_string(), "v".to_string())],
        Vec::new(),
    )
    .ok()
    .map(|request| request.headers()[0].0.clone())
}

/// The name a request can carry into the gateway for these bytes: hyper
/// parses HTTP/1 names with `from_bytes`, and the HTTP/2 decoder uses
/// `from_lowercase`, which also admits `"`.
fn http_header_name(bytes: &[u8]) -> Option<String> {
    axum::http::HeaderName::from_bytes(bytes)
        .or_else(|_| axum::http::HeaderName::from_lowercase(bytes))
        .ok()
        .map(|name| name.as_str().to_owned())
}

#[test]
fn request_header_names_match_what_http_header_name_accepts() {
    use zeroclaw_api::webhook::MAX_PLUGIN_WEBHOOK_HEADER_NAME_BYTES;

    for byte in 0..=u8::MAX {
        let name = char::from(byte).to_string();
        assert_eq!(
            ingress_header_name(&name),
            http_header_name(name.as_bytes()),
            "byte {byte:#04x}"
        );
        if !byte.is_ascii() {
            // Only ASCII has a one-byte UTF-8 form, and http refuses the raw
            // byte as well.
            assert_eq!(http_header_name(&[byte]), None, "byte {byte:#04x}");
        }
    }

    let longest = "n".repeat(MAX_PLUGIN_WEBHOOK_HEADER_NAME_BYTES);
    assert_eq!(ingress_header_name(&longest), Some(longest.clone()));
    assert_eq!(http_header_name(longest.as_bytes()), Some(longest.clone()));
    let too_long = format!("{longest}n");
    assert_eq!(ingress_header_name(&too_long), None);
    assert_eq!(http_header_name(too_long.as_bytes()), None);
}

#[tokio::test]
async fn plugin_webhook_forwards_a_quoted_header_name_that_http2_admits() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let ingress = test_ingress();
    let lease = ingress.registry().start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([(
        "fixture".to_string(),
        fixture_route(sink)
    )])));
    let (seen, observed) = tokio::sync::oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards request");
        let _ = seen.send(request.headers.clone());
        let _ = request.reply.send(Ok(WebhookOutcome::Ack));
    });

    let mut request = plugin_webhook_request(
        "fixture",
        "body",
        SocketAddr::from(([127, 0, 0, 1], 31000)),
        None,
    );
    // The HTTP/1 parser refuses this name; the HTTP/2 decoder builds it this
    // way.
    let quoted = axum::http::HeaderName::from_lowercase(b"x-\"quoted\"")
        .expect("HTTP/2 admits a quote in a header name");
    request
        .headers_mut()
        .insert(quoted, HeaderValue::from_static("v"));
    let response = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), ingress)
        .oneshot(request)
        .await
        .expect("plugin route is infallible");
    assert_eq!(response.status(), StatusCode::OK);
    let headers = observed.await.expect("worker records the request");
    assert!(
        headers.contains(&("x-\"quoted\"".to_string(), "v".to_string())),
        "{headers:?}"
    );
}

#[tokio::test]
async fn outcome_response_gives_every_outcome_its_fixed_response() {
    const TEXT: &str = "text/plain; charset=utf-8";

    for outcome in [
        PluginWebhookOutcome::Ack,
        PluginWebhookOutcome::Reply("challenge".to_string()),
        PluginWebhookOutcome::NotFound,
        PluginWebhookOutcome::QueueFull,
        PluginWebhookOutcome::Unavailable,
        PluginWebhookOutcome::Unauthorized,
        PluginWebhookOutcome::BadRequest,
        PluginWebhookOutcome::InvalidResponse,
        PluginWebhookOutcome::Timeout,
        PluginWebhookOutcome::Cancelled,
    ] {
        let (status, content_type, body) = match &outcome {
            PluginWebhookOutcome::Ack => (StatusCode::OK, None, ""),
            PluginWebhookOutcome::Reply(reply) => (StatusCode::OK, Some(TEXT), reply.as_str()),
            PluginWebhookOutcome::NotFound => {
                (StatusCode::NOT_FOUND, Some(TEXT), "webhook not found")
            }
            PluginWebhookOutcome::QueueFull => (
                StatusCode::TOO_MANY_REQUESTS,
                Some(TEXT),
                "webhook queue full",
            ),
            PluginWebhookOutcome::Unavailable | PluginWebhookOutcome::Cancelled => (
                StatusCode::SERVICE_UNAVAILABLE,
                Some(TEXT),
                "webhook unavailable",
            ),
            PluginWebhookOutcome::Unauthorized => {
                (StatusCode::UNAUTHORIZED, Some(TEXT), "unauthorized webhook")
            }
            PluginWebhookOutcome::BadRequest => {
                (StatusCode::BAD_REQUEST, Some(TEXT), "invalid webhook")
            }
            PluginWebhookOutcome::InvalidResponse => (
                StatusCode::BAD_GATEWAY,
                Some(TEXT),
                "invalid webhook response",
            ),
            PluginWebhookOutcome::Timeout => (
                StatusCode::GATEWAY_TIMEOUT,
                Some(TEXT),
                "webhook processing timed out",
            ),
        };
        let shown = format!("{outcome:?}");
        let response = outcome_response(outcome.clone());
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
