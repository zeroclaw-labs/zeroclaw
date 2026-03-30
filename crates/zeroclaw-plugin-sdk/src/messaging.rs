//! Messaging module — wraps send_message and get_channels host functions.
//!
//! Each function serializes a typed request to JSON, calls the corresponding
//! host function via Extism shared memory, and deserializes the JSON response.

use extism_pdk::*;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Request / response types (mirror the host-side structs)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    channel: String,
    recipient: String,
    message: String,
}

#[derive(Debug, Deserialize)]
struct SendMessageResponse {
    success: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GetChannelsResponse {
    #[serde(default)]
    channels: Vec<String>,
    #[serde(default)]
    error: Option<String>,
}

// ---------------------------------------------------------------------------
// Host function imports
// ---------------------------------------------------------------------------

#[host_fn]
extern "ExtismHost" {
    fn zeroclaw_send_message(input: Json<SendMessageRequest>) -> Json<SendMessageResponse>;
    fn zeroclaw_get_channels(input: Json<()>) -> Json<GetChannelsResponse>;
}

// ---------------------------------------------------------------------------
// Public wrapper API
// ---------------------------------------------------------------------------

/// Send a message to a recipient on the given channel.
pub fn send(channel: &str, recipient: &str, message: &str) -> Result<(), Error> {
    let request = SendMessageRequest {
        channel: channel.to_string(),
        recipient: recipient.to_string(),
        message: message.to_string(),
    };
    let Json(response) = unsafe { zeroclaw_send_message(Json(request))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    if !response.success {
        return Err(Error::msg("send_message returned success=false"));
    }
    Ok(())
}

/// Get the list of available channel names.
pub fn get_channels() -> Result<Vec<String>, Error> {
    let Json(response) = unsafe { zeroclaw_get_channels(Json(()))? };
    if let Some(err) = response.error {
        return Err(Error::msg(err));
    }
    Ok(response.channels)
}
