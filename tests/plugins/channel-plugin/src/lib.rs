use extism_pdk::*;
use serde::{Deserialize, Serialize};

// --- Guest-side DTOs matching the channel guest interface contract ---

/// Mirrors the host-side SendMessage for the Extism boundary.
#[derive(Deserialize)]
struct SendMessageInput {
    content: String,
    recipient: String,
    #[allow(dead_code)]
    subject: Option<String>,
    #[allow(dead_code)]
    thread_ts: Option<String>,
}

/// Response from channel_send.
#[derive(Serialize)]
struct SendResponse {
    success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// A single channel message event.
#[derive(Serialize)]
struct ChannelMessageOutput {
    id: String,
    sender: String,
    reply_target: String,
    content: String,
    channel: String,
    timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    thread_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    interruption_scope_id: Option<String>,
}

/// Response from channel_listen.
#[derive(Serialize)]
struct ListenResponse {
    messages: Vec<ChannelMessageOutput>,
}

/// Accepts a SendMessage JSON, logs/buffers it, and returns success.
#[plugin_fn]
pub fn channel_send(input: String) -> FnResult<String> {
    let msg: SendMessageInput = serde_json::from_str(&input)?;

    // Echo back success with the content as confirmation
    let response = SendResponse {
        success: true,
        error: None,
    };

    // Store the sent message content in plugin var so channel_listen can echo it back
    var::set("last_sent_content", &msg.content)?;
    var::set("last_sent_recipient", &msg.recipient)?;

    Ok(serde_json::to_string(&response)?)
}

/// Produces synthetic ChannelMessage events as JSON.
/// Returns a batch of test messages. If a message was previously sent via channel_send,
/// it echoes that back as the first message.
#[plugin_fn]
pub fn channel_listen(_input: String) -> FnResult<String> {
    let mut messages = Vec::new();

    // If a message was sent via channel_send, echo it back
    if let Ok(Some(content)) = var::get::<String>("last_sent_content") {
        let sender = var::get::<String>("last_sent_recipient")
            .ok()
            .flatten()
            .unwrap_or_else(|| "unknown".to_string());
        messages.push(ChannelMessageOutput {
            id: "echo-1".to_string(),
            sender,
            reply_target: "wasm-channel".to_string(),
            content,
            channel: "wasm-test".to_string(),
            timestamp: 1000,
            thread_ts: None,
            interruption_scope_id: None,
        });
    }

    // Always include a synthetic test message
    messages.push(ChannelMessageOutput {
        id: "synthetic-1".to_string(),
        sender: "test-bot".to_string(),
        reply_target: "wasm-channel".to_string(),
        content: "hello from wasm channel plugin".to_string(),
        channel: "wasm-test".to_string(),
        timestamp: 2000,
        thread_ts: None,
        interruption_scope_id: None,
    });

    let response = ListenResponse { messages };
    Ok(serde_json::to_string(&response)?)
}
