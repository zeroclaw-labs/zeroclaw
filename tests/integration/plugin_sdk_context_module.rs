//! Verify that the zeroclaw-plugin-sdk context module wraps session,
//! user_identity, and agent_config host functions.
//!
//! Acceptance criterion for US-ZCL-27:
//! > context module wraps session user_identity and agent_config

use std::path::Path;

const SDK_DIR: &str = "crates/zeroclaw-plugin-sdk";

fn sdk_context_source() -> String {
    let base = Path::new(env!("CARGO_MANIFEST_DIR")).join(SDK_DIR);
    let context_rs = base.join("src/context.rs");
    assert!(
        context_rs.is_file(),
        "zeroclaw-plugin-sdk/src/context.rs is missing — cannot verify context wrappers"
    );
    std::fs::read_to_string(&context_rs).expect("failed to read context.rs")
}

// ---------------------------------------------------------------------------
// Wrapper function existence
// ---------------------------------------------------------------------------

#[test]
fn context_module_has_session_function() {
    let src = sdk_context_source();
    assert!(
        src.contains("fn session") || src.contains("fn get_session"),
        "context module must expose a session wrapper function"
    );
}

#[test]
fn context_module_has_user_identity_function() {
    let src = sdk_context_source();
    assert!(
        src.contains("fn user_identity") || src.contains("fn get_user_identity"),
        "context module must expose a user_identity wrapper function"
    );
}

#[test]
fn context_module_has_agent_config_function() {
    let src = sdk_context_source();
    assert!(
        src.contains("fn agent_config") || src.contains("fn get_agent_config"),
        "context module must expose an agent_config wrapper function"
    );
}

// ---------------------------------------------------------------------------
// Host function imports — the module must declare the extern host functions
// ---------------------------------------------------------------------------

#[test]
fn context_module_imports_zeroclaw_context_session() {
    let src = sdk_context_source();
    assert!(
        src.contains("context_session") || src.contains("zeroclaw_get_session"),
        "context module must reference the context_session host function"
    );
}

#[test]
fn context_module_imports_zeroclaw_context_user_identity() {
    let src = sdk_context_source();
    assert!(
        src.contains("context_user_identity") || src.contains("zeroclaw_get_user_identity"),
        "context module must reference the context_user_identity host function"
    );
}

#[test]
fn context_module_imports_zeroclaw_context_agent_config() {
    let src = sdk_context_source();
    assert!(
        src.contains("context_agent_config") || src.contains("zeroclaw_get_agent_config"),
        "context module must reference the context_agent_config host function"
    );
}

// ---------------------------------------------------------------------------
// The wrappers should use typed response structs (JSON ABI)
// ---------------------------------------------------------------------------

#[test]
fn context_module_uses_session_response_struct() {
    let src = sdk_context_source();
    assert!(
        src.contains("SessionContext") || src.contains("session_id"),
        "session wrapper should deserialize a typed response with session info"
    );
}

#[test]
fn context_module_uses_user_identity_response_struct() {
    let src = sdk_context_source();
    assert!(
        src.contains("UserIdentity") || (src.contains("user") && src.contains("identity")),
        "user_identity wrapper should deserialize a typed response with user identity info"
    );
}

#[test]
fn context_module_uses_agent_config_response_struct() {
    let src = sdk_context_source();
    assert!(
        src.contains("AgentConfig") || (src.contains("agent") && src.contains("config")),
        "agent_config wrapper should deserialize a typed response with agent config info"
    );
}

// ---------------------------------------------------------------------------
// Public API — all three wrappers should be pub
// ---------------------------------------------------------------------------

#[test]
fn context_session_is_public() {
    let src = sdk_context_source();
    assert!(
        src.contains("pub fn session") || src.contains("pub fn get_session"),
        "session wrapper must be pub so plugin authors can call it"
    );
}

#[test]
fn context_user_identity_is_public() {
    let src = sdk_context_source();
    assert!(
        src.contains("pub fn user_identity") || src.contains("pub fn get_user_identity"),
        "user_identity wrapper must be pub so plugin authors can call it"
    );
}

#[test]
fn context_agent_config_is_public() {
    let src = sdk_context_source();
    assert!(
        src.contains("pub fn agent_config") || src.contains("pub fn get_agent_config"),
        "agent_config wrapper must be pub so plugin authors can call it"
    );
}
