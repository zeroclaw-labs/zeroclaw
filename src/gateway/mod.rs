use crate::channels::{Channel, WhatsAppChannel};
use crate::config::Config;
use crate::memory::{self, Memory, MemoryCategory};
use crate::providers::{self, Provider};
use crate::security::pairing::{constant_time_eq, is_public_bind, PairingGuard};
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Run a minimal HTTP gateway (webhook + health check)
/// Zero new dependencies — uses raw TCP + tokio.
#[allow(clippy::too_many_lines)]
pub async fn run_gateway(host: &str, port: u16, config: Config) -> Result<()> {
    // ── Security: refuse public bind without tunnel or explicit opt-in ──
    if is_public_bind(host) && config.tunnel.provider == "none" && !config.gateway.allow_public_bind
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
        println!("  � PAIRING REQUIRED — use this one-time code:");
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

    loop {
        let (mut stream, peer) = listener.accept().await?;
        let provider = provider.clone();
        let model = model.clone();
        let mem = mem.clone();
        let auto_save = config.memory.auto_save;
        let secret = webhook_secret.clone();
        let pairing = pairing.clone();
        let whatsapp = whatsapp_channel.clone();

        tokio::spawn(async move {
            // Read with 30s timeout to prevent slow-loris attacks
            let mut buf = vec![0u8; 65_536]; // 64KB max request
            let n = match tokio::time::timeout(Duration::from_secs(30), stream.read(&mut buf)).await
            {
                Ok(Ok(n)) if n > 0 => n,
                _ => return,
            };

            let request = String::from_utf8_lossy(&buf[..n]);
            let first_line = request.lines().next().unwrap_or("");
            let parts: Vec<&str> = first_line.split_whitespace().collect();

            if let [method, path, ..] = parts.as_slice() {
                tracing::info!("{peer} → {method} {path}");
                handle_request(
                    &mut stream,
                    method,
                    path,
                    &request,
                    &provider,
                    &model,
                    temperature,
                    &mem,
                    auto_save,
                    secret.as_ref(),
                    &pairing,
                    whatsapp.as_ref(),
                )
                .await;
            } else {
                let _ = send_response(&mut stream, 400, "Bad Request").await;
            }
        });
    }
}

/// Extract a header value from a raw HTTP request.
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

#[allow(clippy::too_many_arguments)]
async fn handle_request(
    stream: &mut tokio::net::TcpStream,
    method: &str,
    path: &str,
    request: &str,
    provider: &Arc<dyn Provider>,
    model: &str,
    temperature: f64,
    mem: &Arc<dyn Memory>,
    auto_save: bool,
    webhook_secret: Option<&Arc<str>>,
    pairing: &PairingGuard,
    whatsapp: Option<&Arc<WhatsAppChannel>>,
) {
    match (method, path) {
        // Health check — always public (no secrets leaked)
        ("GET", "/health") => {
            let body = serde_json::json!({
                "status": "ok",
                "paired": pairing.is_paired(),
                "runtime": crate::health::snapshot_json(),
            });
            let _ = send_json(stream, 200, &body).await;
        }

        // Pairing endpoint — exchange one-time code for bearer token
        ("POST", "/pair") => {
            let code = extract_header(request, "X-Pairing-Code").unwrap_or("");
            match pairing.try_pair(code) {
                Ok(Some(token)) => {
                    tracing::info!("🔐 New client paired successfully");
                    let body = serde_json::json!({
                        "paired": true,
                        "token": token,
                        "message": "Save this token — use it as Authorization: Bearer <token>"
                    });
                    let _ = send_json(stream, 200, &body).await;
                }
                Ok(None) => {
                    tracing::warn!("🔐 Pairing attempt with invalid code");
                    let err = serde_json::json!({"error": "Invalid pairing code"});
                    let _ = send_json(stream, 403, &err).await;
                }
                Err(lockout_secs) => {
                    tracing::warn!(
                        "🔐 Pairing locked out — too many failed attempts ({lockout_secs}s remaining)"
                    );
                    let err = serde_json::json!({
                        "error": format!("Too many failed attempts. Try again in {lockout_secs}s."),
                        "retry_after": lockout_secs
                    });
                    let _ = send_json(stream, 429, &err).await;
                }
            }
        }

        // WhatsApp webhook verification (Meta sends GET to verify)
        ("GET", "/whatsapp") => {
            handle_whatsapp_verify(stream, request, whatsapp).await;
        }

        // WhatsApp incoming message webhook
        ("POST", "/whatsapp") => {
            handle_whatsapp_message(
                stream,
                request,
                provider,
                model,
                temperature,
                mem,
                auto_save,
                whatsapp,
            )
            .await;
        }

        ("POST", "/webhook") => {
            // ── Bearer token auth (pairing) ──
            if pairing.require_pairing() {
                let auth = extract_header(request, "Authorization").unwrap_or("");
                let token = auth.strip_prefix("Bearer ").unwrap_or("");
                if !pairing.is_authenticated(token) {
                    tracing::warn!("Webhook: rejected — not paired / invalid bearer token");
                    let err = serde_json::json!({
                        "error": "Unauthorized — pair first via POST /pair, then send Authorization: Bearer <token>"
                    });
                    let _ = send_json(stream, 401, &err).await;
                    return;
                }
            }

            // ── Webhook secret auth (optional, additional layer) ──
            if let Some(secret) = webhook_secret {
                let header_val = extract_header(request, "X-Webhook-Secret");
                match header_val {
                    Some(val) if constant_time_eq(val, secret.as_ref()) => {}
                    _ => {
                        tracing::warn!(
                            "Webhook: rejected request — invalid or missing X-Webhook-Secret"
                        );
                        let err = serde_json::json!({"error": "Unauthorized — invalid or missing X-Webhook-Secret header"});
                        let _ = send_json(stream, 401, &err).await;
                        return;
                    }
                }
            }
            handle_webhook(
                stream,
                request,
                provider,
                model,
                temperature,
                mem,
                auto_save,
            )
            .await;
        }

        _ => {
            let body = serde_json::json!({
                "error": "Not found",
                "routes": ["GET /health", "POST /pair", "POST /webhook"]
            });
            let _ = send_json(stream, 404, &body).await;
        }
    }
}

async fn handle_webhook(
    stream: &mut tokio::net::TcpStream,
    request: &str,
    provider: &Arc<dyn Provider>,
    model: &str,
    temperature: f64,
    mem: &Arc<dyn Memory>,
    auto_save: bool,
) {
    let body_str = request
        .split("\r\n\r\n")
        .nth(1)
        .or_else(|| request.split("\n\n").nth(1))
        .unwrap_or("");

    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(body_str) else {
        let err = serde_json::json!({"error": "Invalid JSON. Expected: {\"message\": \"...\"}"});
        let _ = send_json(stream, 400, &err).await;
        return;
    };

    let Some(message) = parsed.get("message").and_then(|v| v.as_str()) else {
        let err = serde_json::json!({"error": "Missing 'message' field in JSON"});
        let _ = send_json(stream, 400, &err).await;
        return;
    };

    if auto_save {
        let _ = mem
            .store("webhook_msg", message, MemoryCategory::Conversation)
            .await;
    }

    match provider.chat(message, model, temperature).await {
        Ok(response) => {
            let body = serde_json::json!({"response": response, "model": model});
            let _ = send_json(stream, 200, &body).await;
        }
        Err(e) => {
            let err = serde_json::json!({"error": format!("LLM error: {e}")});
            let _ = send_json(stream, 500, &err).await;
        }
    }
}

/// Handle webhook verification (GET /whatsapp)
/// Meta sends: `GET /whatsapp?hub.mode=subscribe&hub.verify_token=<token>&hub.challenge=<challenge>`
async fn handle_whatsapp_verify(
    stream: &mut tokio::net::TcpStream,
    request: &str,
    whatsapp: Option<&Arc<WhatsAppChannel>>,
) {
    let Some(wa) = whatsapp else {
        let err = serde_json::json!({"error": "WhatsApp not configured"});
        let _ = send_json(stream, 404, &err).await;
        return;
    };

    // Parse query string from the request line
    // GET /whatsapp?hub.mode=subscribe&hub.verify_token=xxx&hub.challenge=yyy HTTP/1.1
    let first_line = request.lines().next().unwrap_or("");
    let query = first_line
        .split_whitespace()
        .nth(1)
        .and_then(|path| path.split('?').nth(1))
        .unwrap_or("");

    let mut mode = None;
    let mut token = None;
    let mut challenge = None;

    for pair in query.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            match key {
                "hub.mode" => mode = Some(value),
                "hub.verify_token" => token = Some(value),
                "hub.challenge" => challenge = Some(value),
                _ => {}
            }
        }
    }

    // Verify the token matches
    if mode == Some("subscribe") && token == Some(wa.verify_token()) {
        if let Some(ch) = challenge {
            // URL-decode the challenge (basic: replace %XX)
            let decoded = urlencoding_decode(ch);
            tracing::info!("WhatsApp webhook verified successfully");
            let _ = send_response(stream, 200, &decoded).await;
        } else {
            let _ = send_response(stream, 400, "Missing hub.challenge").await;
        }
    } else {
        tracing::warn!("WhatsApp webhook verification failed — token mismatch");
        let _ = send_response(stream, 403, "Forbidden").await;
    }
}

/// Simple URL decoding (handles %XX sequences)
fn urlencoding_decode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '%' {
            let hex: String = chars.by_ref().take(2).collect();
            // Require exactly 2 hex digits for valid percent encoding
            if hex.len() == 2 {
                if let Ok(byte) = u8::from_str_radix(&hex, 16) {
                    result.push(byte as char);
                } else {
                    result.push('%');
                    result.push_str(&hex);
                }
            } else {
                // Incomplete percent encoding - preserve as-is
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

/// Handle incoming message webhook (POST /whatsapp)
#[allow(clippy::too_many_arguments)]
async fn handle_whatsapp_message(
    stream: &mut tokio::net::TcpStream,
    request: &str,
    provider: &Arc<dyn Provider>,
    model: &str,
    temperature: f64,
    mem: &Arc<dyn Memory>,
    auto_save: bool,
    whatsapp: Option<&Arc<WhatsAppChannel>>,
) {
    let Some(wa) = whatsapp else {
        let err = serde_json::json!({"error": "WhatsApp not configured"});
        let _ = send_json(stream, 404, &err).await;
        return;
    };

    // Extract JSON body
    let body_str = request
        .split("\r\n\r\n")
        .nth(1)
        .or_else(|| request.split("\n\n").nth(1))
        .unwrap_or("");

    let Ok(payload) = serde_json::from_str::<serde_json::Value>(body_str) else {
        let err = serde_json::json!({"error": "Invalid JSON payload"});
        let _ = send_json(stream, 400, &err).await;
        return;
    };

    // Parse messages from the webhook payload
    let messages = wa.parse_webhook_payload(&payload);

    if messages.is_empty() {
        // Acknowledge the webhook even if no messages (could be status updates)
        let _ = send_response(stream, 200, "OK").await;
        return;
    }

    // Process each message
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

        // Auto-save to memory
        if auto_save {
            let _ = mem
                .store(
                    &format!("whatsapp_{}", msg.sender),
                    &msg.content,
                    MemoryCategory::Conversation,
                )
                .await;
        }

        // Call the LLM
        match provider.chat(&msg.content, model, temperature).await {
            Ok(response) => {
                // Send reply via WhatsApp
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

    // Acknowledge the webhook
    let _ = send_response(stream, 200, "OK").await;
}

async fn send_response(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &str,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await
}

async fn send_json(
    stream: &mut tokio::net::TcpStream,
    status: u16,
    body: &serde_json::Value,
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let json = serde_json::to_string(body).unwrap_or_default();
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{json}",
        json.len()
    );
    stream.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener as TokioListener;

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
        // OS typically assigns ephemeral ports >= 1024
        assert!(
            actual >= 1024,
            "Random port {actual} should be >= 1024 (unprivileged)"
        );
    }

    #[tokio::test]
    async fn specific_port_binds_exactly() {
        // Find a free port first via port 0, then rebind to it
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

        // Spawn a client that connects
        let client = tokio::spawn(async move {
            tokio::net::TcpStream::connect(format!("127.0.0.1:{actual_port}"))
                .await
                .unwrap()
        });

        // Accept the connection
        let (stream, _peer) = listener.accept().await.unwrap();
        assert!(stream.peer_addr().is_ok());
        client.await.unwrap();
    }

    #[tokio::test]
    async fn duplicate_specific_port_fails() {
        let l1 = TokioListener::bind("127.0.0.1:0").await.unwrap();
        let port = l1.local_addr().unwrap().port();
        // Try to bind the same port while l1 is still alive
        let result = TokioListener::bind(format!("127.0.0.1:{port}")).await;
        assert!(result.is_err(), "Binding an already-used port must fail");
    }

    #[tokio::test]
    async fn tunnel_gets_actual_port_not_zero() {
        // Simulate what run_gateway does: bind port 0, extract actual port
        let port: u16 = 0;
        let host = "127.0.0.1";
        let listener = TokioListener::bind(format!("{host}:{port}")).await.unwrap();
        let actual_port = listener.local_addr().unwrap().port();

        // This is the port that would be passed to tun.start(host, actual_port)
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
        // split_once on ':' means only the first colon splits key/value
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

    // ── URL decoding tests ────────────────────────────────────

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
        // Invalid hex should be preserved
        assert_eq!(urlencoding_decode("%ZZ"), "%ZZ");
        assert_eq!(urlencoding_decode("%G1"), "%G1");
    }

    #[test]
    fn urlencoding_decode_incomplete_percent() {
        // Incomplete percent encoding at end - function takes available chars
        // "%2" -> takes "2" as hex, fails to parse, outputs "%2"
        assert_eq!(urlencoding_decode("test%2"), "test%2");
        // "%" alone -> takes "" as hex, fails to parse, outputs "%"
        assert_eq!(urlencoding_decode("test%"), "test%");
    }

    #[test]
    fn urlencoding_decode_challenge_token() {
        // Typical Meta webhook challenge
        assert_eq!(urlencoding_decode("1234567890"), "1234567890");
    }

    #[test]
    fn urlencoding_decode_unicode_percent() {
        // URL-encoded UTF-8 bytes for emoji (simplified test)
        assert_eq!(urlencoding_decode("%41%42%43"), "ABC");
    }

    }
