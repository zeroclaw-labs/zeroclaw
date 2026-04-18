//! Deep end-to-end integration tests for the One2X gateway routing IoC
//! pattern.
//!
//! The unit tests in `src/one2x.rs` exercise the route-extender closure
//! via `tower::ServiceExt::oneshot` (in-memory service dispatch). That's
//! fast and catches most regressions but doesn't cover the full HTTP path:
//! real TCP socket → axum::serve → router → handler → response bytes on
//! the wire → HTTP client parse. These integration tests do the full
//! network roundtrip, catching regressions like:
//!
//! - A route registered but never reachable because of middleware ordering.
//! - SSE-framed responses that parse in-memory but break on the wire.
//! - Concurrent request handling regressions.
//!
//! We use `Router<()>` (no state) to avoid constructing the full `AppState`,
//! which is a prohibitively large dependency tree for tests. The
//! closure-composition pattern we verify is structurally identical
//! regardless of state type.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::Duration;

use axum::response::IntoResponse;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use serde_json::json;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;

/// Spawn a background axum server bound to an OS-assigned port and return
/// its address + a cancellation handle. The server is torn down when the
/// `JoinHandle` is aborted.
async fn spawn_test_server(router: Router<()>) -> (SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let handle = tokio::spawn(async move {
        let _ = axum::serve(listener, router.into_make_service()).await;
    });
    // Give axum a tick to start listening.
    tokio::time::sleep(Duration::from_millis(25)).await;
    (addr, handle)
}

/// Simulate `src/one2x/mod.rs::register_gateway_routes` — build a router
/// that has POST /agent (SSE stream), POST /agent/clear (JSON), and
/// GET /ws/channel (stub, returns 200 OK since we can't easily test WS
/// without the real pairing stack).
fn build_one2x_router() -> Router<()> {
    let extender: Box<dyn Fn(Router<()>) -> Router<()> + Send + Sync> = Box::new(|router| {
        router
            .route("/agent", post(test_agent_sse_handler))
            .route("/agent/clear", post(test_agent_clear_handler))
            .route("/ws/channel", get(test_ws_stub_handler))
    });
    extender(Router::new())
}

/// Minimal SSE handler that streams three `chunk` events + one `done` event,
/// mirroring what `src/one2x/agent_sse.rs::handle_agent_sse` does at the
/// event-framing level.
async fn test_agent_sse_handler() -> impl IntoResponse {
    let (tx, rx) = tokio::sync::mpsc::channel::<Event>(8);
    tokio::spawn(async move {
        for chunk in ["hello ", "from ", "sse"] {
            let _ = tx
                .send(
                    Event::default()
                        .event("chunk")
                        .data(json!({ "content": chunk }).to_string()),
                )
                .await;
        }
        let _ = tx
            .send(
                Event::default()
                    .event("done")
                    .data(json!({ "session_id": "test-session" }).to_string()),
            )
            .await;
    });
    let stream = ReceiverStream::new(rx).map(|e| Ok::<Event, Infallible>(e));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn test_agent_clear_handler(Json(body): Json<serde_json::Value>) -> Json<serde_json::Value> {
    Json(json!({ "cleared": body["session_id"] }))
}

async fn test_ws_stub_handler() -> impl IntoResponse {
    (axum::http::StatusCode::OK, "ws-stub-ok")
}

/// Full SSE client: connect via HTTP, stream chunks, assert sequence.
#[tokio::test]
async fn sse_endpoint_streams_chunks_over_real_tcp() {
    let (addr, server) = spawn_test_server(build_one2x_router()).await;

    let client = reqwest::Client::builder()
        .build()
        .expect("build reqwest client");

    let response = client
        .post(format!("http://{addr}/agent"))
        .json(&json!({ "message": "hi" }))
        .send()
        .await
        .expect("POST /agent");
    assert_eq!(response.status(), 200, "SSE endpoint should return 200");
    assert!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.starts_with("text/event-stream"))
            .unwrap_or(false),
        "SSE responses must carry text/event-stream content-type: {:?}",
        response.headers().get("content-type")
    );

    // Consume the SSE stream as raw bytes and verify the framing.
    let body = response.bytes_stream();
    let mut body = Box::pin(body);
    let mut full = Vec::<u8>::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.expect("stream chunk readable");
        full.extend_from_slice(&chunk);
    }
    let text = String::from_utf8_lossy(&full);
    // SSE frames are `event: <name>\ndata: <payload>\n\n`. Assert the
    // events we scheduled appear in order.
    assert!(text.contains("event: chunk"), "missing chunk event: {text}");
    assert!(text.contains("\"hello \""), "first chunk payload: {text}");
    assert!(text.contains("\"from \""), "second chunk payload: {text}");
    assert!(text.contains("\"sse\""), "third chunk payload: {text}");
    assert!(text.contains("event: done"), "missing done event: {text}");
    assert!(
        text.contains("test-session"),
        "done event should carry session_id: {text}"
    );
    // Frame boundaries — each event ends with \n\n.
    let frame_count = text.matches("\n\n").count();
    assert!(
        frame_count >= 4,
        "expected at least 4 SSE frame boundaries (3 chunk + 1 done), got {frame_count}: {text}"
    );

    server.abort();
}

/// JSON POST endpoint: exercises the non-streaming path and the router's
/// body-extractor integration.
#[tokio::test]
async fn clear_endpoint_returns_json_over_real_tcp() {
    let (addr, server) = spawn_test_server(build_one2x_router()).await;

    let client = reqwest::Client::new();
    let response: serde_json::Value = client
        .post(format!("http://{addr}/agent/clear"))
        .json(&json!({ "session_id": "abc-123" }))
        .send()
        .await
        .expect("POST /agent/clear")
        .json()
        .await
        .expect("parse JSON");
    assert_eq!(response["cleared"], "abc-123");

    server.abort();
}

/// Unknown path returns 404 even though the extender was applied — confirms
/// we haven't accidentally added a catchall or wildcard.
#[tokio::test]
async fn unknown_route_returns_404_over_real_tcp() {
    let (addr, server) = spawn_test_server(build_one2x_router()).await;

    let status = reqwest::Client::new()
        .get(format!("http://{addr}/definitely-not-a-route"))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 404);

    server.abort();
}

/// Concurrent requests hit multiple routes in parallel — validates the
/// router doesn't serialize requests and that route dispatch is stable
/// under contention.
#[tokio::test]
async fn concurrent_requests_to_multiple_routes() {
    let (addr, server) = spawn_test_server(build_one2x_router()).await;
    let client = reqwest::Client::new();

    let mut handles = Vec::new();
    for i in 0..8 {
        let c = client.clone();
        let a = addr;
        handles.push(tokio::spawn(async move {
            if i % 2 == 0 {
                // Even: /agent/clear
                let resp = c
                    .post(format!("http://{a}/agent/clear"))
                    .json(&json!({ "session_id": format!("sid-{i}") }))
                    .send()
                    .await
                    .expect("send")
                    .json::<serde_json::Value>()
                    .await
                    .expect("json");
                assert_eq!(resp["cleared"], format!("sid-{i}"));
            } else {
                // Odd: /ws/channel stub
                let txt = c
                    .get(format!("http://{a}/ws/channel"))
                    .send()
                    .await
                    .expect("send")
                    .text()
                    .await
                    .expect("text");
                assert_eq!(txt, "ws-stub-ok");
            }
        }));
    }
    for h in handles {
        h.await.expect("task join");
    }

    server.abort();
}

/// When no extender is applied (empty Router), all one2x routes 404 — this
/// is the other half of the IoC contract: no silent default wiring.
#[tokio::test]
async fn empty_router_has_no_one2x_routes() {
    let router: Router<()> = Router::new();
    let (addr, server) = spawn_test_server(router).await;

    let status = reqwest::Client::new()
        .post(format!("http://{addr}/agent"))
        .json(&json!({ "message": "hi" }))
        .send()
        .await
        .expect("request")
        .status();
    assert_eq!(status, 404);

    server.abort();
}
