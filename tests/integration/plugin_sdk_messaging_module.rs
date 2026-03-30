//! Verify that the zeroclaw-plugin-sdk messaging module wraps send_message and
//! get_channels host functions.
//!
//! Acceptance criterion for US-ZCL-27:
//! > messaging module wraps send_message and get_channels

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";

fn sdk_messaging_source() -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let messaging_rs = base.join("src/messaging.rs");
    assert!(
        messaging_rs.is_file(),
        "zeroclaw-plugin-sdk/src/messaging.rs is missing — cannot verify messaging wrappers"
    );
    std::fs::read_to_string(&messaging_rs).expect("failed to read messaging.rs")
}

// ---------------------------------------------------------------------------
// Wrapper function existence
// ---------------------------------------------------------------------------

#[test]
fn messaging_module_has_send_message_function() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("fn send_message") || src.contains("fn send"),
        "messaging module must expose a send_message wrapper function"
    );
}

#[test]
fn messaging_module_has_get_channels_function() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("fn get_channels") || src.contains("fn channels"),
        "messaging module must expose a get_channels wrapper function"
    );
}

// ---------------------------------------------------------------------------
// Host function imports — the module must declare the extern host functions
// ---------------------------------------------------------------------------

#[test]
fn messaging_module_imports_zeroclaw_send_message() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("zeroclaw_send_message"),
        "messaging module must reference the zeroclaw_send_message host function"
    );
}

#[test]
fn messaging_module_imports_zeroclaw_get_channels() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("zeroclaw_get_channels"),
        "messaging module must reference the zeroclaw_get_channels host function"
    );
}

// ---------------------------------------------------------------------------
// The wrappers should use typed request/response structs (JSON ABI)
// ---------------------------------------------------------------------------

#[test]
fn messaging_module_uses_send_request_struct() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("ChannelSendRequest")
            || (src.contains("channel") && src.contains("recipient") && src.contains("message")),
        "send_message wrapper should serialize a typed request with channel, recipient, and message fields"
    );
}

#[test]
fn messaging_module_uses_send_response_struct() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("ChannelSendResponse") || src.contains("success"),
        "send_message wrapper should deserialize a typed response with a success field"
    );
}

#[test]
fn messaging_module_uses_get_channels_response_struct() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("GetChannelsResponse") || src.contains("channels"),
        "get_channels wrapper should deserialize a typed response with a channels field"
    );
}

// ---------------------------------------------------------------------------
// Public API — both wrappers should be pub
// ---------------------------------------------------------------------------

#[test]
fn messaging_send_message_is_public() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("pub fn send_message") || src.contains("pub fn send"),
        "send_message wrapper must be pub so plugin authors can call it"
    );
}

#[test]
fn messaging_get_channels_is_public() {
    let src = sdk_messaging_source();
    assert!(
        src.contains("pub fn get_channels") || src.contains("pub fn channels"),
        "get_channels wrapper must be pub so plugin authors can call it"
    );
}
