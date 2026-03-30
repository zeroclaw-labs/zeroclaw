//! Device-link WebSocket client for MoA Tauri app.
//!
//! Maintains a persistent WebSocket connection to the Railway gateway's
//! `/ws/device-link` endpoint. This enables:
//!   - Web chat → local device relay (browser user messages)
//!   - Channel chat → local device relay (KakaoTalk, WhatsApp, etc.)
//!   - Hybrid relay (LLM via proxy, tools locally)
//!   - Remote check_key probes
//!
//! The client handles reconnection, heartbeat, and message dispatching.

use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// How often to send heartbeat pings (seconds).
const HEARTBEAT_INTERVAL_SECS: u64 = 30;

/// Reconnection delay after disconnect (seconds).
const RECONNECT_DELAY_SECS: u64 = 5;

/// Max reconnection delay (exponential backoff cap).
const MAX_RECONNECT_DELAY_SECS: u64 = 60;

/// Start the device-link client loop.
///
/// This function runs forever (until `stop_flag` is set), maintaining a
/// WebSocket connection to the Railway gateway. On disconnect, it
/// automatically reconnects with exponential backoff.
///
/// # Arguments
/// - `relay_url`: Railway server URL (e.g., "https://api.mymoa.app")
/// - `token`: Session authentication token
/// - `device_id`: This device's unique ID
/// - `local_gateway_url`: Local ZeroClaw gateway URL for agent processing
/// - `connected_flag`: Shared flag indicating connection status
/// - `stop_flag`: Set to true to stop the client
pub async fn run_device_link_client(
    relay_url: String,
    token: String,
    device_id: String,
    local_gateway_url: String,
    connected_flag: Arc<AtomicBool>,
    stop_flag: Arc<AtomicBool>,
) {
    let mut backoff_secs = RECONNECT_DELAY_SECS;

    loop {
        if stop_flag.load(Ordering::Relaxed) {
            tracing::info!("Device-link client: stop requested");
            break;
        }

        if token.is_empty() {
            tracing::debug!("Device-link client: no token, waiting...");
            tokio::time::sleep(tokio::time::Duration::from_secs(RECONNECT_DELAY_SECS)).await;
            continue;
        }

        // Build WebSocket URL
        let ws_url = build_ws_url(&relay_url, &token, &device_id);
        tracing::info!(url = ws_url.as_str(), "Device-link: connecting...");

        match connect_async(&ws_url).await {
            Ok((ws_stream, _response)) => {
                tracing::info!("Device-link: connected to Railway");
                connected_flag.store(true, Ordering::Relaxed);
                backoff_secs = RECONNECT_DELAY_SECS; // Reset backoff

                // Run the message loop
                handle_connection(
                    ws_stream,
                    &local_gateway_url,
                    &token,
                    &stop_flag,
                )
                .await;

                connected_flag.store(false, Ordering::Relaxed);
                tracing::info!("Device-link: disconnected");
            }
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    backoff_secs = backoff_secs,
                    "Device-link: connection failed, retrying..."
                );
            }
        }

        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        // Exponential backoff
        tokio::time::sleep(tokio::time::Duration::from_secs(backoff_secs)).await;
        backoff_secs = (backoff_secs * 2).min(MAX_RECONNECT_DELAY_SECS);
    }

    connected_flag.store(false, Ordering::Relaxed);
    tracing::info!("Device-link client: stopped");
}

/// Build the WebSocket URL for device-link connection.
fn build_ws_url(relay_url: &str, token: &str, device_id: &str) -> String {
    let base = relay_url
        .replace("https://", "wss://")
        .replace("http://", "ws://");
    let base = base.trim_end_matches('/');
    format!(
        "{}/ws/device-link?token={}&device_id={}",
        base,
        urlencoded(token),
        urlencoded(device_id)
    )
}

/// Simple URL encoding for query parameter values.
fn urlencoded(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

/// Handle a single WebSocket connection session.
///
/// Uses an outbound mpsc channel so that the heartbeat task and message
/// handlers never contend on the WebSocket sink directly.
async fn handle_connection(
    ws_stream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    local_gateway_url: &str,
    token: &str,
    stop_flag: &AtomicBool,
) {
    let (ws_sender, mut ws_receiver) = ws_stream.split();

    // Outbound channel: heartbeat + response handlers send here; a dedicated
    // task drains it to the actual WebSocket sink.
    let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::channel::<Message>(64);

    // Sender task — sole owner of ws_sender
    let sender_task = tokio::spawn(async move {
        let mut ws_sender = ws_sender;
        while let Some(msg) = outbound_rx.recv().await {
            if ws_sender.send(msg).await.is_err() {
                break;
            }
        }
    });

    // Heartbeat task — sends Ping via the outbound channel
    let heartbeat_tx = outbound_tx.clone();
    let stop_clone = Arc::new(AtomicBool::new(false));
    let stop_for_heartbeat = stop_clone.clone();
    let heartbeat_task = tokio::spawn(async move {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(HEARTBEAT_INTERVAL_SECS)).await;
            if stop_for_heartbeat.load(Ordering::Relaxed) {
                break;
            }
            if heartbeat_tx.send(Message::Ping(vec![])).await.is_err() {
                break;
            }
        }
    });

    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(120))
        .build()
        .unwrap_or_default();

    // Main message loop
    while let Some(msg_result) = ws_receiver.next().await {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }

        let msg = match msg_result {
            Ok(Message::Text(text)) => text.to_string(),
            Ok(Message::Ping(data)) => {
                let _ = outbound_tx.send(Message::Pong(data)).await;
                continue;
            }
            Ok(Message::Close(_)) => {
                tracing::info!("Device-link: server closed connection");
                break;
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::warn!(error = %e, "Device-link: receive error");
                break;
            }
        };

        // Parse incoming JSON message
        let parsed: serde_json::Value = match serde_json::from_str(&msg) {
            Ok(v) => v,
            Err(_) => continue,
        };

        let msg_type = parsed["type"].as_str().unwrap_or("");
        let msg_id = parsed["id"].as_str().unwrap_or("").to_string();
        let content_str = parsed["content"].as_str().unwrap_or("");

        match msg_type {
            // ── check_key: Railway asking if we have an LLM API key ──
            "check_key" => {
                let content: serde_json::Value =
                    serde_json::from_str(content_str).unwrap_or_default();
                let provider = content["check_provider"].as_str().unwrap_or("gemini");
                let has_key = check_local_api_key(provider, local_gateway_url).await;

                let response = serde_json::json!({
                    "type": "check_key_response",
                    "id": msg_id,
                    "has_key": has_key,
                });
                let _ = outbound_tx
                    .send(Message::Text(response.to_string().into()))
                    .await;
            }

            // ── remote_message: full relay (user has local LLM key) ──
            "remote_message" => {
                let response_text = process_on_local_gateway(
                    &http_client,
                    local_gateway_url,
                    content_str,
                    None, // no proxy — use local key
                    None,
                    None,
                )
                .await;

                send_response(&outbound_tx, &msg_id, &response_text).await;
            }

            // ── hybrid_relay: no LLM key, use proxy token for LLM ──
            "hybrid_relay" => {
                let content: serde_json::Value =
                    serde_json::from_str(content_str).unwrap_or_default();
                let user_content = content["content"].as_str().unwrap_or("");
                let proxy_token = content["proxy_token"].as_str();
                let proxy_url = content["proxy_url"].as_str();
                let provider = content["provider"].as_str();

                let response_text = process_on_local_gateway(
                    &http_client,
                    local_gateway_url,
                    user_content,
                    proxy_url,
                    proxy_token,
                    provider,
                )
                .await;

                send_response(&outbound_tx, &msg_id, &response_text).await;
            }

            // ── channel_relay: channel message (KakaoTalk, WhatsApp, etc.) ──
            "channel_relay" => {
                let content: serde_json::Value =
                    serde_json::from_str(content_str).unwrap_or_default();
                let user_content = content["content"].as_str().unwrap_or("");
                let proxy_token = content["proxy_token"].as_str();
                let proxy_url = content["proxy_url"].as_str();
                let provider = content["provider"].as_str();
                let session_id = content["session_id"].as_str();

                let response_text = process_on_local_gateway_with_session(
                    &http_client,
                    local_gateway_url,
                    user_content,
                    proxy_url,
                    proxy_token,
                    provider,
                    session_id,
                    token,
                )
                .await;

                send_response(&outbound_tx, &msg_id, &response_text).await;
            }

            _ => {
                tracing::debug!(msg_type = msg_type, "Device-link: unknown message type");
            }
        }
    }

    stop_clone.store(true, Ordering::Relaxed);
    heartbeat_task.abort();
    // Drop outbound_tx so the sender task finishes
    drop(outbound_tx);
    let _ = sender_task.await;
}

/// Send a response (done or error) back to Railway via the outbound channel.
async fn send_response(
    outbound_tx: &tokio::sync::mpsc::Sender<Message>,
    msg_id: &str,
    response_text: &str,
) {
    let response = serde_json::json!({
        "type": "remote_response",
        "id": msg_id,
        "content": response_text,
    });
    let _ = outbound_tx
        .send(Message::Text(response.to_string().into()))
        .await;
}

/// Check if the local gateway has an API key for the given provider.
async fn check_local_api_key(provider: &str, local_gateway_url: &str) -> bool {
    // Try to read the local config to check for API keys
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(3))
        .build()
        .unwrap_or_default();

    // Check via health endpoint or config endpoint
    let url = format!("{}/health", local_gateway_url);
    if client.get(&url).send().await.is_err() {
        return false; // Gateway not running
    }

    // Check ZeroClaw config for provider key
    let home = dirs_next::home_dir().unwrap_or_default();
    let config_path = home.join(".zeroclaw").join("config.toml");
    if let Ok(config_str) = std::fs::read_to_string(&config_path) {
        if let Ok(config) = config_str.parse::<toml::Table>() {
            // Check api_key
            if let Some(key) = config.get("api_key").and_then(|v| v.as_str()) {
                if !key.trim().is_empty() {
                    return true;
                }
            }
            // Check provider_api_keys map
            if let Some(keys) = config.get("provider_api_keys").and_then(|v| v.as_table()) {
                if let Some(key) = keys.get(provider).and_then(|v| v.as_str()) {
                    if !key.trim().is_empty() {
                        return true;
                    }
                }
            }
        }
    }

    // Check environment variables as fallback
    let env_var = match provider {
        "anthropic" => "ANTHROPIC_API_KEY",
        "openai" => "OPENAI_API_KEY",
        "gemini" | "google" | "google-gemini" => "GEMINI_API_KEY",
        "perplexity" => "PERPLEXITY_API_KEY",
        _ => return false,
    };
    std::env::var(env_var)
        .ok()
        .is_some_and(|v| !v.trim().is_empty())
}

/// Process a message on the local ZeroClaw gateway via /api/chat.
async fn process_on_local_gateway(
    client: &reqwest::Client,
    gateway_url: &str,
    content: &str,
    proxy_url: Option<&str>,
    proxy_token: Option<&str>,
    provider: Option<&str>,
) -> String {
    process_on_local_gateway_with_session(
        client,
        gateway_url,
        content,
        proxy_url,
        proxy_token,
        provider,
        None,
        "",
    )
    .await
}

/// Process a message on the local ZeroClaw gateway with session support.
async fn process_on_local_gateway_with_session(
    client: &reqwest::Client,
    gateway_url: &str,
    content: &str,
    proxy_url: Option<&str>,
    proxy_token: Option<&str>,
    provider: Option<&str>,
    session_id: Option<&str>,
    auth_token: &str,
) -> String {
    let mut body = serde_json::json!({
        "message": content,
    });

    if let Some(proxy_url) = proxy_url {
        body["proxy_url"] = serde_json::Value::String(proxy_url.to_string());
    }
    if let Some(proxy_token) = proxy_token {
        body["proxy_token"] = serde_json::Value::String(proxy_token.to_string());
    }
    if let Some(provider) = provider {
        body["provider"] = serde_json::Value::String(provider.to_string());
    }
    if let Some(session_id) = session_id {
        body["session_id"] = serde_json::Value::String(session_id.to_string());
    }

    let url = format!("{}/api/chat", gateway_url);
    let result = client
        .post(&url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", auth_token))
        .json(&body)
        .send()
        .await;

    match result {
        Ok(resp) => {
            if resp.status().is_success() {
                match resp.json::<serde_json::Value>().await {
                    Ok(json) => json["response"]
                        .as_str()
                        .unwrap_or("Processing complete.")
                        .to_string(),
                    Err(_) => "Response received but could not parse.".to_string(),
                }
            } else {
                let status = resp.status();
                let error_text = resp.text().await.unwrap_or_default();
                tracing::warn!(
                    status = %status,
                    error = error_text.as_str(),
                    "Local gateway returned error"
                );
                format!("Local processing error ({}): {}", status, error_text)
            }
        }
        Err(e) => {
            tracing::error!(error = %e, "Failed to reach local gateway");
            format!("Could not reach local MoA gateway: {}", e)
        }
    }
}
