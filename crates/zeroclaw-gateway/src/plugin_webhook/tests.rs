use super::*;
use std::collections::HashMap;

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
use zeroclaw_api::webhook::WebhookOutcome;

fn plugin_webhook_test_router(
    state: AppState,
    registry: Arc<zeroclaw_api::webhook::PluginWebhookRegistry>,
) -> Router {
    routes(registry)
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
    use zeroclaw_api::webhook::PluginWebhookRegistry;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let registry = Arc::new(PluginWebhookRegistry::new());
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    let registry_lease = registry.start_generation();
    assert!(registry_lease.replace(HashMap::from([("fixture".to_string(), sink)])));
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
    let response = plugin_webhook_test_router(state, registry)
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
    use zeroclaw_api::webhook::{PluginWebhookRegistry, WebhookReject};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let registry = Arc::new(PluginWebhookRegistry::new());
    let (sink, mut receiver) = tokio::sync::mpsc::channel(3);
    let registry_lease = registry.start_generation();
    assert!(registry_lease.replace(HashMap::from([("fixture".to_string(), sink)])));
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
    let app = plugin_webhook_test_router(state, registry);
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
    use zeroclaw_api::webhook::PluginWebhookRegistry;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let registry = Arc::new(PluginWebhookRegistry::new());
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    let registry_lease = registry.start_generation();
    assert!(registry_lease.replace(HashMap::from([("fixture".to_string(), sink)])));
    let (cancelled, observed) = tokio::sync::oneshot::channel();
    zeroclaw_spawn::spawn!(async move {
        let request = receiver.recv().await.expect("route forwards request");
        request.cancellation.cancelled().await;
        let _ = cancelled.send(());
    });

    let response = plugin_webhook_test_router(state, registry)
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
    use zeroclaw_api::webhook::{PluginWebhookRegistry, RawWebhook};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let state = admin_paircode_state(&tmp, false, false);
    let peer = SocketAddr::from(([203, 0, 113, 10], 30_303));

    let unknown_registry = Arc::new(PluginWebhookRegistry::new());
    let unknown = plugin_webhook_test_router(state.clone(), unknown_registry)
        .oneshot(plugin_webhook_request("missing", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);

    let full_registry = Arc::new(PluginWebhookRegistry::new());
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
    let full_registry_lease = full_registry.start_generation();
    assert!(full_registry_lease.replace(HashMap::from([("full".to_string(), full_sink)])));
    let full = plugin_webhook_test_router(state.clone(), full_registry)
        .oneshot(plugin_webhook_request("full", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response_text(full).await, "webhook queue full");
    let _ = full_receiver.recv().await;

    let closed_registry = Arc::new(PluginWebhookRegistry::new());
    let (closed_sink, closed_receiver) = tokio::sync::mpsc::channel(1);
    drop(closed_receiver);
    let closed_registry_lease = closed_registry.start_generation();
    assert!(closed_registry_lease.replace(HashMap::from([("closed".to_string(), closed_sink)])));
    let closed = plugin_webhook_test_router(state.clone(), closed_registry)
        .oneshot(plugin_webhook_request("closed", "body", peer, None))
        .await
        .expect("plugin route is infallible");
    assert_eq!(closed.status(), StatusCode::SERVICE_UNAVAILABLE);

    let body_registry = Arc::new(PluginWebhookRegistry::new());
    let (body_sink, mut body_receiver) = tokio::sync::mpsc::channel(1);
    let body_registry_lease = body_registry.start_generation();
    assert!(body_registry_lease.replace(HashMap::from([("fixture".to_string(), body_sink)])));
    let oversized = plugin_webhook_test_router(state, body_registry)
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
    use zeroclaw_api::webhook::PluginWebhookRegistry;

    async fn acknowledge(
        mut receiver: tokio::sync::mpsc::Receiver<zeroclaw_api::webhook::RawWebhook>,
    ) {
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
    let registry = Arc::new(PluginWebhookRegistry::new());
    let (sink, receiver) = tokio::sync::mpsc::channel(4);
    let registry_lease = registry.start_generation();
    assert!(registry_lease.replace(HashMap::from([("fixture".to_string(), sink)])));
    zeroclaw_spawn::spawn!(acknowledge(receiver));
    let app = plugin_webhook_test_router(state, registry);

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
    let trusted_registry = Arc::new(PluginWebhookRegistry::new());
    let (trusted_sink, trusted_receiver) = tokio::sync::mpsc::channel(4);
    let trusted_registry_lease = trusted_registry.start_generation();
    assert!(trusted_registry_lease.replace(HashMap::from([("fixture".to_string(), trusted_sink)])));
    zeroclaw_spawn::spawn!(acknowledge(trusted_receiver));
    let trusted = plugin_webhook_test_router(trusted_state, trusted_registry);
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
    use zeroclaw_api::webhook::PluginWebhookRegistry;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let registry = Arc::new(PluginWebhookRegistry::new());
    let lease = registry.start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(2);
    assert!(lease.replace(HashMap::from([("fixture".to_string(), sink)])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), registry);
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
    use zeroclaw_api::webhook::PluginWebhookRegistry;

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let registry = Arc::new(PluginWebhookRegistry::new());
    let lease = registry.start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([("fixture".to_string(), sink)])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), registry);
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
    use zeroclaw_api::webhook::{MAX_WEBHOOK_RESPONSE_BODY_BYTES, PluginWebhookRegistry};

    let tmp = tempfile::TempDir::new().expect("temp dir");
    let registry = Arc::new(PluginWebhookRegistry::new());
    let lease = registry.start_generation();
    let (sink, mut receiver) = tokio::sync::mpsc::channel(1);
    assert!(lease.replace(HashMap::from([("fixture".to_string(), sink)])));
    let app = plugin_webhook_test_router(admin_paircode_state(&tmp, false, false), registry);
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
async fn plugin_webhook_idempotency_waits_for_owner_outcome_and_fences_stale_tokens() {
    use zeroclaw_api::webhook::{WebhookReservation, WebhookReservationStatus};

    let store = Arc::new(IdempotencyStore::new(Duration::from_secs(300), 8));
    let idempotency = plugin_webhook_idempotency(Arc::clone(&store), "fixture");
    let first = match idempotency.begin("stable-id") {
        WebhookReservation::Owner(token) => token,
        _ => panic!("first request must own the reservation"),
    };
    let mut duplicate = match idempotency.begin("stable-id") {
        WebhookReservation::InFlight(waiter) => waiter,
        _ => panic!("duplicate must observe an in-flight owner"),
    };

    assert!(idempotency.rollback(&first));
    assert_eq!(duplicate.wait().await, WebhookReservationStatus::RolledBack);
    let replacement = match idempotency.begin("stable-id") {
        WebhookReservation::Owner(token) => token,
        _ => panic!("duplicate must acquire after owner rollback"),
    };
    assert_ne!(first.generation(), replacement.generation());
    assert!(!idempotency.rollback(&first));

    let mut committed_duplicate = match idempotency.begin("stable-id") {
        WebhookReservation::InFlight(waiter) => waiter,
        _ => panic!("later duplicate must wait for replacement owner"),
    };
    assert!(idempotency.commit(&replacement));
    assert_eq!(
        committed_duplicate.wait().await,
        WebhookReservationStatus::Committed
    );
    assert!(matches!(
        idempotency.begin("stable-id"),
        WebhookReservation::Committed
    ));
}

#[test]
fn plugin_webhook_pending_capacity_does_not_starve_existing_idempotency_callers() {
    use zeroclaw_api::webhook::WebhookReservation;

    let store = Arc::new(IdempotencyStore::new(Duration::from_secs(300), 1));
    let idempotency = plugin_webhook_idempotency(Arc::clone(&store), "fixture");
    let owner = match idempotency.begin("first") {
        WebhookReservation::Owner(token) => token,
        _ => panic!("first plugin delivery owns the pending slot"),
    };
    assert!(matches!(
        idempotency.begin("second"),
        WebhookReservation::Unavailable
    ));
    assert!(
        store.record_if_new("native-webhook-key"),
        "pending plugin work must not be misreported as a duplicate on the existing webhook path"
    );
    assert!(idempotency.rollback(&owner));

    let replacement = match idempotency.begin("second") {
        WebhookReservation::Owner(token) => token,
        _ => panic!("rolling back frees the bounded pending slot"),
    };
    assert!(idempotency.commit(&replacement));
    let entries = store.entries.lock();
    assert_eq!(entries.pending.len(), 0);
    assert_eq!(entries.committed.len(), 1);
    assert!(entries.committed.contains_key(replacement.key()));
}
