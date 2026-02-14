use crate::channels::{Channel, WhatsAppChannel};
use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::providers::{self, Provider};
use crate::security::pairing::{constant_time_eq, PairingGuard};
use anyhow::Result;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::timeout::TimeoutLayer;

// ── Shared state ────────────────────────────────────────────────────

#[derive(Clone)]
struct AppState {
    provider: Arc<dyn Provider>,
    model: String,
    temperature: f64,
    mem: Arc<dyn Memory>,
    auto_save: bool,
    webhook_secret: Option<Arc<str>>,
    pairing: Arc<PairingGuard>,
    whatsapp: Option<Arc<WhatsAppChannel>>,
}

// ── Helpers ─────────────────────────────────────────────────────────

fn json_response(status: StatusCode, body: serde_json::Value) -> Response {
    (status, Json(body)).into_response()
}

fn text_response(status: StatusCode, body: &str) -> Response {
    (status, body.to_owned()).into_response()
}

// ── Request logging middleware ──────────────────────────────────────

async fn log_request(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: axum::extract::Request,
    next: middleware::Next,
) -> Response {
    tracing::info!("{peer} → {} {}", req.method(), req.uri().path());
    next.run(req).await
}

// ── Route handlers ──────────────────────────────────────────────────

async fn health_handler(State(state): State<AppState>) -> Response {
    let body = serde_json::json!({
        "status": "ok",
        "paired": state.pairing.is_paired(),
        "runtime": crate::health::snapshot_json(),
    });
    json_response(StatusCode::OK, body)
}

async fn pair_handler(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let code = headers
        .get("x-pairing-code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    match state.pairing.try_pair(code) {
        Ok(Some(token)) => {
            tracing::info!("🔐 New client paired successfully");
            json_response(
                StatusCode::OK,
                serde_json::json!({
                    "paired": true,
                    "token": token,
                    "message": "Save this token — use it as Authorization: Bearer <token>"
                }),
            )
        }
        Ok(None) => {
            tracing::warn!("🔐 Pairing attempt with invalid code");
            json_response(
                StatusCode::FORBIDDEN,
                serde_json::json!({"error": "Invalid pairing code"}),
            )
        }
        Err(lockout_secs) => {
            tracing::warn!(
                "🔐 Pairing locked out — too many failed attempts ({lockout_secs}s remaining)"
            );
            json_response(
                StatusCode::TOO_MANY_REQUESTS,
                serde_json::json!({
                    "error": format!("Too many failed attempts. Try again in {lockout_secs}s."),
                    "retry_after": lockout_secs
                }),
            )
        }
    }
}

#[derive(Deserialize)]
struct WhatsAppVerifyQuery {
    #[serde(rename = "hub.mode")]
    mode: Option<String>,
    #[serde(rename = "hub.verify_token")]
    verify_token: Option<String>,
    #[serde(rename = "hub.challenge")]
    challenge: Option<String>,
}

async fn whatsapp_verify_handler(
    State(state): State<AppState>,
    Query(query): Query<WhatsAppVerifyQuery>,
) -> Response {
    let Some(wa) = state.whatsapp.as_ref() else {
        return json_response(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "WhatsApp not configured"}),
        );
    };

    if query.mode.as_deref() == Some("subscribe")
        && query.verify_token.as_deref() == Some(wa.verify_token())
    {
        if let Some(challenge) = query.challenge {
            tracing::info!("WhatsApp webhook verified successfully");
            text_response(StatusCode::OK, &challenge)
        } else {
            text_response(StatusCode::BAD_REQUEST, "Missing hub.challenge")
        }
    } else {
        tracing::warn!("WhatsApp webhook verification failed — token mismatch");
        text_response(StatusCode::FORBIDDEN, "Forbidden")
    }
}

async fn whatsapp_message_handler(State(state): State<AppState>, body: String) -> Response {
    let Some(wa) = state.whatsapp.as_ref() else {
        return json_response(
            StatusCode::NOT_FOUND,
            serde_json::json!({"error": "WhatsApp not configured"}),
        );
    };

    let Ok(payload) = serde_json::from_str::<serde_json::Value>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Invalid JSON payload"}),
        );
    };

    let messages = wa.parse_webhook_payload(&payload);

    if messages.is_empty() {
        return text_response(StatusCode::OK, "OK");
    }

    for msg in &messages {
        tracing::info!(
            "WhatsApp message from {}: {}",
            msg.sender,
            if msg.content.len() > 50 {
                format!("{}...", &msg.content[..50])
            } else {
                msg.content.clone()
            }
        );

        if state.auto_save {
            let _ = state
                .mem
                .store(
                    &format!("whatsapp_{}", msg.sender),
                    &msg.content,
                    MemoryCategory::Conversation,
                )
                .await;
        }

        match state
            .provider
            .chat(&msg.content, &state.model, state.temperature)
            .await
        {
            Ok(response) => {
                if let Err(e) = wa.send(&response, &msg.sender).await {
                    tracing::error!("Failed to send WhatsApp reply: {e}");
                }
            }
            Err(e) => {
                tracing::error!("LLM error for WhatsApp message: {e}");
                let _ = wa.send(&format!("⚠️ Error: {e}"), &msg.sender).await;
            }
        }
    }

    text_response(StatusCode::OK, "OK")
}

async fn webhook_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> Response {
    // ── Bearer token auth (pairing) ──
    if state.pairing.require_pairing() {
        let auth = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        let token = auth.strip_prefix("Bearer ").unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            tracing::warn!("Webhook: rejected — not paired / invalid bearer token");
            return json_response(
                StatusCode::UNAUTHORIZED,
                serde_json::json!({
                    "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
                }),
            );
        }
    }

    // ── Webhook secret auth (optional, additional layer) ──
    if let Some(ref secret) = state.webhook_secret {
        let header_val = headers
            .get("x-webhook-secret")
            .and_then(|v| v.to_str().ok());
        match header_val {
            Some(val) if constant_time_eq(val, secret.as_ref()) => {}
            _ => {
                tracing::warn!(
                    "Webhook: rejected request — invalid or missing X-Webhook-Secret"
                );
                return json_response(
                    StatusCode::UNAUTHORIZED,
                    serde_json::json!({"error": "Unauthorized — invalid or missing X-Webhook-Secret header"}),
                );
            }
        }
    }

    // ── Parse and process ──
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&body) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Invalid JSON. Expected: {\"message\": \"...\"}"}),
        );
    };

    let Some(message) = parsed.get("message").and_then(|v| v.as_str()) else {
        return json_response(
            StatusCode::BAD_REQUEST,
            serde_json::json!({"error": "Missing 'message' field in JSON"}),
        );
    };

    if state.auto_save {
        let _ = state
            .mem
            .store("webhook_msg", message, MemoryCategory::Conversation)
            .await;
    }

    match state
        .provider
        .chat(message, &state.model, state.temperature)
        .await
    {
        Ok(response) => json_response(
            StatusCode::OK,
            serde_json::json!({"response": response, "model": state.model}),
        ),
        Err(e) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            serde_json::json!({"error": format!("LLM error: {e}")}),
        ),
    }
}

async fn fallback_handler() -> Response {
    json_response(
        StatusCode::NOT_FOUND,
        serde_json::json!({
            "error": "Not found",
            "routes": ["GET /health", "POST /pair", "POST /webhook"]
        }),
    )
}

// ── Public entry point ──────────────────────────────────────────────

/// Run an HTTP gateway (webhook + health check) backed by axum.
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(host: &str, port: u16, config: Config) -> Result<()> {
    // ── Security: refuse public bind without tunnel or explicit opt-in ──
    if crate::security::pairing::is_public_bind(host)
        && config.tunnel.provider == "none"
        && !config.gateway.allow_public_bind
    {
        anyhow::bail!(
            "🛑 Refusing to bind to {host} — gateway would be exposed to the internet.\n\
             Fix: use --host 127.0.0.1 (default), configure a tunnel, or set\n\
             [gateway] allow_public_bind = true in config.toml (NOT recommended)."
        );
    }

    let listener = TcpListener::bind(format!("{host}:{port}")).await?;
    let actual_port = listener.local_addr()?.port();
    let addr = format!("{host}:{actual_port}");

    let provider: Arc<dyn Provider> = Arc::from(providers::create_resilient_provider(
        config.default_provider.as_deref().unwrap_or("openrouter"),
        config.api_key.as_deref(),
        &config.reliability,
    )?);
    let model = config
        .default_model
        .clone()
        .unwrap_or_else(|| "anthropic/claude-sonnet-4-20250514".into());
    let temperature = config.default_temperature;
    let mem: Arc<dyn Memory> = Arc::from(memory::create_memory(
        &config.memory,
        &config.workspace_dir,
        config.api_key.as_deref(),
    )?);

    // Extract webhook secret for authentication
    let webhook_secret: Option<Arc<str>> = config
        .channels_config
        .webhook
        .as_ref()
        .and_then(|w| w.secret.as_deref())
        .map(Arc::from);

    // WhatsApp channel (if configured)
    let whatsapp_channel: Option<Arc<WhatsAppChannel>> =
        config.channels_config.whatsapp.as_ref().map(|wa| {
            Arc::new(WhatsAppChannel::new(
                wa.access_token.clone(),
                wa.phone_number_id.clone(),
                wa.verify_token.clone(),
                wa.allowed_numbers.clone(),
            ))
        });

    // ── Pairing guard ──────────────────────────────────────
    let pairing = Arc::new(PairingGuard::new(
        config.gateway.require_pairing,
        &config.gateway.paired_tokens,
    ));

    // ── Tunnel ────────────────────────────────────────────────
    let tunnel = crate::tunnel::create_tunnel(&config.tunnel)?;
    let mut tunnel_url: Option<String> = None;

    if let Some(ref tun) = tunnel {
        println!("🔗 Starting {} tunnel...", tun.name());
        match tun.start(host, actual_port).await {
            Ok(url) => {
                println!("🌐 Tunnel active: {url}");
                tunnel_url = Some(url);
            }
            Err(e) => {
                println!("⚠️  Tunnel failed to start: {e}");
                println!("   Falling back to local-only mode.");
            }
        }
    }

    println!("🦀 ZeroClaw Gateway listening on http://{addr}");
    if let Some(ref url) = tunnel_url {
        println!("  🌐 Public URL: {url}");
    }
    println!("  POST /pair      — pair a new client (X-Pairing-Code header)");
    println!("  POST /webhook   — {{\"message\": \"your prompt\"}}");
    if whatsapp_channel.is_some() {
        println!("  GET  /whatsapp  — Meta webhook verification");
        println!("  POST /whatsapp  — WhatsApp message webhook");
    }
    println!("  GET  /health    — health check");
    if let Some(code) = pairing.pairing_code() {
        println!();
        println!("  🔐 PAIRING REQUIRED — use this one-time code:");
        println!("     ┌──────────────┐");
        println!("     │  {code}  │");
        println!("     └──────────────┘");
        println!("     Send: POST /pair with header X-Pairing-Code: {code}");
    } else if pairing.require_pairing() {
        println!("  🔒 Pairing: ACTIVE (bearer token required)");
    } else {
        println!("  ⚠️  Pairing: DISABLED (all requests accepted)");
    }
    if webhook_secret.is_some() {
        println!("  🔒 Webhook secret: ENABLED");
    }
    println!("  Press Ctrl+C to stop.\n");

    crate::health::mark_component_ok("gateway");

    // ── Build axum app ────────────────────────────────────────
    let state = AppState {
        provider,
        model,
        temperature,
        mem,
        auto_save: config.memory.auto_save,
        webhook_secret,
        pairing,
        whatsapp: whatsapp_channel,
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .route("/pair", post(pair_handler))
        .route(
            "/whatsapp",
            get(whatsapp_verify_handler).post(whatsapp_message_handler),
        )
        .route("/webhook", post(webhook_handler))
        .fallback(fallback_handler)
        .layer(middleware::from_fn(log_request))
        .layer(TimeoutLayer::with_status_code(StatusCode::REQUEST_TIMEOUT, Duration::from_secs(60)))
        .layer(RequestBodyLimitLayer::new(65_536))
        .with_state(state);

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

// ── Test-only helpers (moved from production code) ──────────────────

#[cfg(test)]
fn extract_header<'a>(request: &'a str, header_name: &str) -> Option<&'a str> {
    let lower_name = header_name.to_lowercase();
    for line in request.lines() {
        if let Some((key, value)) = line.split_once(':') {
            if key.trim().to_lowercase() == lower_name {
                return Some(value.trim());
            }
        }
    }
    None
}

#[cfg(test)]
fn urlencoding_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                } else {
                    result.push('%');
                    result.push_str(&hex);
                }
            } else {
                result.push('%');
                result.push_str(&hex);
            }
        } else if c == '+' {
            result.push(' ');
        } else {
            result.push(c);
        }
    }

    result
}

// ── Tests ───────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use http_body_util::BodyExt;
    use tokio::net::TcpListener as TokioListener;
    use tower::ServiceExt;

    // ── Mock implementations for test state ──────────────────

    struct MockProvider;

    #[async_trait::async_trait]
    impl Provider for MockProvider {
        async fn chat_with_system(
            &self,
            _system_prompt: Option<&str>,
            message: &str,
            _model: &str,
            _temperature: f64,
        ) -> anyhow::Result<String> {
            Ok(format!("echo: {message}"))
        }
    }

    struct MockMemory;

    #[async_trait::async_trait]
    impl Memory for MockMemory {
        fn name(&self) -> &str {
            "mock"
        }
        async fn store(
            &self,
            _key: &str,
            _content: &str,
            _category: MemoryCategory,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn recall(
            &self,
            _query: &str,
            _limit: usize,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(vec![])
        }
        async fn get(
            &self,
            _key: &str,
        ) -> anyhow::Result<Option<crate::memory::MemoryEntry>> {
            Ok(None)
        }
        async fn list(
            &self,
            _category: Option<&MemoryCategory>,
        ) -> anyhow::Result<Vec<crate::memory::MemoryEntry>> {
            Ok(vec![])
        }
        async fn forget(&self, _key: &str) -> anyhow::Result<bool> {
            Ok(false)
        }
        async fn count(&self) -> anyhow::Result<usize> {
            Ok(0)
        }
        async fn health_check(&self) -> bool {
            true
        }
    }

    fn test_state() -> AppState {
        AppState {
            provider: Arc::new(MockProvider),
            model: "test-model".into(),
            temperature: 0.7,
            mem: Arc::new(MockMemory),
            auto_save: false,
            webhook_secret: None,
            pairing: Arc::new(PairingGuard::new(false, &[])),
            whatsapp: None,
        }
    }

    fn test_app(state: AppState) -> Router {
        Router::new()
            .route("/health", get(health_handler))
            .route("/pair", post(pair_handler))
            .route(
                "/whatsapp",
                get(whatsapp_verify_handler).post(whatsapp_message_handler),
            )
            .route("/webhook", post(webhook_handler))
            .fallback(fallback_handler)
            .layer(RequestBodyLimitLayer::new(65_536))
            .with_state(state)
    }

    async fn body_json(resp: Response) -> serde_json::Value {
        let bytes = resp.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    // ── Axum integration tests ───────────────────────────────

    #[tokio::test]
    async fn health_returns_200_json() {
        let app = test_app(test_state());
        let req = axum::http::Request::builder()
            .uri("/health")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["status"], "ok");
    }

    #[tokio::test]
    async fn webhook_requires_auth_when_pairing_enabled() {
        let mut state = test_state();
        state.pairing = Arc::new(PairingGuard::new(true, &[]));
        let app = test_app(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"message":"hi"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_rejects_invalid_secret() {
        let mut state = test_state();
        state.webhook_secret = Some(Arc::from("correct-secret"));
        let app = test_app(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .header("x-webhook-secret", "wrong-secret")
            .body(Body::from(r#"{"message":"hi"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn webhook_accepts_valid_secret() {
        let mut state = test_state();
        state.webhook_secret = Some(Arc::from("correct-secret"));
        let app = test_app(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .header("x-webhook-secret", "correct-secret")
            .body(Body::from(r#"{"message":"hello"}"#))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let json = body_json(resp).await;
        assert_eq!(json["response"], "echo: hello");
    }

    #[tokio::test]
    async fn pair_wrong_code_returns_403() {
        let mut state = test_state();
        state.pairing = Arc::new(PairingGuard::new(true, &[]));
        let app = test_app(state);

        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/pair")
            .header("x-pairing-code", "wrong-code")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unknown_route_returns_404_with_routes() {
        let app = test_app(test_state());
        let req = axum::http::Request::builder()
            .uri("/nonexistent")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
        let json = body_json(resp).await;
        assert!(json["routes"].is_array());
    }

    #[tokio::test]
    async fn body_size_limit_returns_413() {
        let app = test_app(test_state());
        let big_body = "x".repeat(65_537); // > 64KB
        let req = axum::http::Request::builder()
            .method("POST")
            .uri("/webhook")
            .header("content-type", "application/json")
            .body(Body::from(big_body))
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn whatsapp_not_configured_returns_404() {
        let app = test_app(test_state());
        let req = axum::http::Request::builder()
            .uri("/whatsapp?hub.mode=subscribe&hub.verify_token=x&hub.challenge=c")
            .body(Body::empty())
            .unwrap();
        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    // ── Port allocation tests ────────────────────────────────

    #[tokio::test]
    async fn port_zero_binds_to_random_port() {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let actual = listener.local_addr().unwrap().port();
        assert_ne!(actual, 0, "OS must assign a non-zero port");
        assert!(actual > 0, "Actual port must be positive");
    }

    #[tokio::test]
    async fn port_zero_assigns_different_ports() {
        let l1 = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let l2 = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let p1 = l1.local_addr().unwrap().port();
        let p2 = l2.local_addr().unwrap().port();
        assert_ne!(p1, p2, "Two port-0 binds should get different ports");
    }

    #[tokio::test]
    async fn port_zero_assigns_high_port() {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let actual = listener.local_addr().unwrap().port();
        assert!(
            actual >= 1024,
            "Random port {actual} should be >= 1024 (unprivileged)"
        );
    }

    #[tokio::test]
    async fn specific_port_binds_exactly() {
        let tmp = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let free_port = tmp.local_addr().unwrap().port();
        drop(tmp);

        let listener = TokioListener::bind(format!("127.0.0.1:{free_port}"))
            .await
            .unwrap();
        let actual = listener.local_addr().unwrap().port();
        assert_eq!(actual, free_port, "Specific port bind must match exactly");
    }

    #[tokio::test]
    async fn actual_port_matches_addr_format() {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();
        let addr = format!("127.0.0.1:{actual_port}");
        assert!(
            addr.starts_with("127.0.0.1:"),
            "Addr format must include host"
        );
        assert!(
            !addr.ends_with(":0"),
            "Addr must not contain port 0 after binding"
        );
    }

    #[tokio::test]
    async fn port_zero_listener_accepts_connections() {
        let listener = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();

        let client = tokio::spawn(async move {
            tokio::net::TcpStream::connect(format!("127.0.0.1:{actual_port}"))
                .await
                .unwrap()
        });

        let (stream, _peer) = listener.accept().await.unwrap();
        assert!(stream.peer_addr().is_ok());
        client.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_specific_port_fails() {
        let l1 = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let port = l1.local_addr().unwrap().port();
        let result = TokioListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(result.is_err(), "Binding an already-used port must fail");
    }

    #[tokio::test]
    async fn tunnel_gets_actual_port_not_zero() {
        let port: u16 = 0;
        let host = "127.0.0.1";
        let listener = TokioListener::bind(format!("{host}:{port}")).await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();

        assert_ne!(actual_port, 0, "Tunnel must receive actual port, not 0");
        assert!(
            actual_port >= 1024,
            "Tunnel port {actual_port} must be unprivileged"
        );
    }

    // ── extract_header tests ─────────────────────────────────

    #[test]
    fn extract_header_finds_value() {
        let req =
            "POST /webhook HTTP/1.1\r\nHost: localhost\r\nX-Webhook-Secret: my-secret\r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some("my-secret"));
    }

    #[test]
    fn extract_header_case_insensitive() {
        let req = "POST /webhook HTTP/1.1\r\nx-webhook-secret: abc123\r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some("abc123"));
    }

    #[test]
    fn extract_header_missing_returns_none() {
        let req = "POST /webhook HTTP/1.1\r\nHost: localhost\r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), None);
    }

    #[test]
    fn extract_header_trims_whitespace() {
        let req = "POST /webhook HTTP/1.1\r\nX-Webhook-Secret:   spaced   \r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some("spaced"));
    }

    #[test]
    fn extract_header_first_match_wins() {
        let req = "POST /webhook HTTP/1.1\r\nX-Webhook-Secret: first\r\nX-Webhook-Secret: second\r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some("first"));
    }

    #[test]
    fn extract_header_empty_value() {
        let req = "POST /webhook HTTP/1.1\r\nX-Webhook-Secret:\r\n\r\n{}";
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some(""));
    }

    #[test]
    fn extract_header_colon_in_value() {
        let req = "POST /webhook HTTP/1.1\r\nAuthorization: Bearer sk-abc:123\r\n\r\n{}";
        assert_eq!(
            extract_header(req, "Authorization"),
            Some("Bearer sk-abc:123")
        );
    }

    #[test]
    fn extract_header_different_header() {
        let req = "POST /webhook HTTP/1.1\r\nContent-Type: application/json\r\nX-Webhook-Secret: mysecret\r\n\r\n{}";
        assert_eq!(
            extract_header(req, "Content-Type"),
            Some("application/json")
        );
        assert_eq!(extract_header(req, "X-Webhook-Secret"), Some("mysecret"));
    }

    #[test]
    fn extract_header_from_empty_request() {
        assert_eq!(extract_header("", "X-Webhook-Secret"), None);
    }

    #[test]
    fn extract_header_newline_only_request() {
        assert_eq!(extract_header("\r\n\r\n", "X-Webhook-Secret"), None);
    }

    // ── URL decoding tests ───────────────────────────────────

    #[test]
    fn urlencoding_decode_plain_text() {
        assert_eq!(urlencoding_decode("hello"), "hello");
    }

    #[test]
    fn urlencoding_decode_spaces() {
        assert_eq!(urlencoding_decode("hello+world"), "hello world");
        assert_eq!(urlencoding_decode("hello%20world"), "hello world");
    }

    #[test]
    fn urlencoding_decode_special_chars() {
        assert_eq!(urlencoding_decode("%21%40%23"), "!@#");
        assert_eq!(urlencoding_decode("%3F%3D%26"), "?=&");
    }

    #[test]
    fn urlencoding_decode_mixed() {
        assert_eq!(urlencoding_decode("hello%20world%21"), "hello world!");
        assert_eq!(urlencoding_decode("a+b%2Bc"), "a b+c");
    }

    #[test]
    fn urlencoding_decode_empty() {
        assert_eq!(urlencoding_decode(""), "");
    }

    #[test]
    fn urlencoding_decode_invalid_hex() {
        assert_eq!(urlencoding_decode("%ZZ"), "%ZZ");
        assert_eq!(urlencoding_decode("%G1"), "%G1");
    }

    #[test]
    fn urlencoding_decode_incomplete_percent() {
        assert_eq!(urlencoding_decode("test%2"), "test%2");
        assert_eq!(urlencoding_decode("test%"), "test%");
    }

    #[test]
    fn urlencoding_decode_challenge_token() {
        assert_eq!(urlencoding_decode("1234567890"), "1234567890");
    }

    #[test]
    fn urlencoding_decode_unicode_percent() {
        assert_eq!(urlencoding_decode("%41%42%43"), "ABC");
    }
}
