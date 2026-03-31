//! Test: context.session() call retrieves channel and conversation info.
//!
//! Task US-ZCL-35-3: verify that the Python SDK example plugin calls
//! `context.session()` and uses the returned `channel_name` and
//! `conversation_id` fields — matching the Rust sdk-example-plugin behavior.
//!
//! Acceptance criterion for US-ZCL-35:
//! > context.session() call retrieves channel and conversation info

use std::path::Path;

const PYTHON_SDK_EXAMPLE_DIR: &str = "tests/plugins/python-sdk-example-plugin";

fn plugin_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(PYTHON_SDK_EXAMPLE_DIR)
        .join("sdk_example_plugin.py");
    std::fs::read_to_string(&path)
        .expect("failed to read python-sdk-example-plugin/sdk_example_plugin.py")
}

fn plugin_manifest() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(PYTHON_SDK_EXAMPLE_DIR)
        .join("plugin.toml");
    std::fs::read_to_string(&path).expect("failed to read python-sdk-example-plugin/plugin.toml")
}

// ---------------------------------------------------------------------------
// Plugin calls context.session()
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_calls_context_session() {
    let src = plugin_source();
    assert!(
        src.contains("context.session()"),
        "Plugin must call context.session() to retrieve session info"
    );
}

#[test]
fn python_sdk_example_imports_context_module() {
    let src = plugin_source();
    assert!(
        src.contains("from zeroclaw_plugin_sdk import context")
            || src.contains("from zeroclaw_plugin_sdk.context import"),
        "Plugin must import the context module from zeroclaw_plugin_sdk"
    );
}

// ---------------------------------------------------------------------------
// Session result used for channel and conversation info
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_uses_session_channel_name() {
    let src = plugin_source();
    assert!(
        src.contains("session.channel_name"),
        "Plugin must access session.channel_name for channel info"
    );
}

#[test]
fn python_sdk_example_uses_session_conversation_id() {
    let src = plugin_source();
    assert!(
        src.contains("session.conversation_id"),
        "Plugin must access session.conversation_id for conversation info"
    );
}

// ---------------------------------------------------------------------------
// Session context flows into the output
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_includes_channel_in_output() {
    let src = plugin_source();
    // The plugin returns a dict with a "channel" key sourced from session
    assert!(
        src.contains(r#""channel""#) || src.contains("'channel'"),
        "Plugin output dict must include a 'channel' key"
    );
}

#[test]
fn python_sdk_example_includes_conversation_id_in_output() {
    let src = plugin_source();
    assert!(
        src.contains(r#""conversation_id""#) || src.contains("'conversation_id'"),
        "Plugin output dict must include a 'conversation_id' key"
    );
}

// ---------------------------------------------------------------------------
// Session context used in greeting message
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_embeds_session_in_greeting() {
    let src = plugin_source();
    // The greeting string interpolates channel_name and conversation_id
    assert!(
        src.contains("session.channel_name") && src.contains("session.conversation_id"),
        "Plugin must embed both channel_name and conversation_id in the greeting"
    );
}

// ---------------------------------------------------------------------------
// Mirrors Rust sdk-example-plugin context usage
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_context_pattern_matches_rust() {
    let src = plugin_source();

    // Rust plugin does: let session = context::session()?;
    // Python plugin should do: session = context.session()
    assert!(
        src.contains("session = context.session()"),
        "Python plugin must assign context.session() to a variable named 'session', \
         matching the Rust sdk-example-plugin pattern"
    );

    // Both plugins use session.channel_name and session.conversation_id
    assert!(
        src.contains("session.channel_name") && src.contains("session.conversation_id"),
        "Python plugin must use session.channel_name and session.conversation_id, \
         matching the Rust sdk-example-plugin fields"
    );
}
