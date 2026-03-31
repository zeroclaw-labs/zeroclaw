//! Test: memory.recall() and memory.store() manage greeting state.
//!
//! Task US-ZCL-35-4: verify that the Python SDK example plugin uses
//! `memory.recall()` and `memory.store()` to track whether a conversation
//! has been greeted before — matching the Rust sdk-example-plugin behavior.
//!
//! Acceptance criterion for US-ZCL-35:
//! > memory.recall() and memory.store() manage greeting state

use std::path::Path;

const PYTHON_SDK_EXAMPLE_DIR: &str = "tests/plugins/python-sdk-example-plugin";

fn plugin_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(PYTHON_SDK_EXAMPLE_DIR)
        .join("sdk_example_plugin.py");
    std::fs::read_to_string(&path)
        .expect("failed to read python-sdk-example-plugin/sdk_example_plugin.py")
}

// ---------------------------------------------------------------------------
// Plugin imports the memory module
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_imports_memory_module() {
    let src = plugin_source();
    // The memory module may be imported standalone or as part of a combined
    // import (e.g. "from zeroclaw_plugin_sdk import context, memory, …")
    assert!(
        src.contains("zeroclaw_plugin_sdk") && src.contains("memory"),
        "Plugin must import the memory module from zeroclaw_plugin_sdk"
    );
}

// ---------------------------------------------------------------------------
// Plugin calls memory.recall() and memory.store()
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_calls_memory_recall() {
    let src = plugin_source();
    assert!(
        src.contains("memory.recall("),
        "Plugin must call memory.recall() to check greeting state"
    );
}

#[test]
fn python_sdk_example_calls_memory_store() {
    let src = plugin_source();
    assert!(
        src.contains("memory.store("),
        "Plugin must call memory.store() to persist greeting state"
    );
}

// ---------------------------------------------------------------------------
// Memory key is derived from conversation context
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_memory_key_uses_conversation_id() {
    let src = plugin_source();
    // The memory key should incorporate session.conversation_id to track
    // per-conversation greeting state
    assert!(
        src.contains("conversation_id"),
        "Memory key must incorporate conversation_id for per-conversation state"
    );
}

#[test]
fn python_sdk_example_memory_key_has_greeted_prefix() {
    let src = plugin_source();
    assert!(
        src.contains("greeted:"),
        "Memory key should use a 'greeted:' prefix to identify greeting state"
    );
}

// ---------------------------------------------------------------------------
// Memory controls first-visit vs return-visit behavior
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_tracks_first_visit() {
    let src = plugin_source();
    assert!(
        src.contains("first_visit"),
        "Plugin must track first_visit state derived from memory.recall()"
    );
}

#[test]
fn python_sdk_example_first_visit_in_output() {
    let src = plugin_source();
    // The output dict should include a first_visit key
    assert!(
        src.contains(r#""first_visit""#) || src.contains("'first_visit'"),
        "Plugin output dict must include a 'first_visit' key"
    );
}

#[test]
fn python_sdk_example_stores_after_first_greeting() {
    let src = plugin_source();
    // memory.store() should be called inside the first_visit branch
    // so the next invocation sees a recalled value
    assert!(
        src.contains("if first_visit"),
        "Plugin must branch on first_visit to decide whether to store greeting state"
    );
}

// ---------------------------------------------------------------------------
// Return visit shows different greeting
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_welcome_back_on_return() {
    let src = plugin_source();
    assert!(
        src.contains("Welcome back"),
        "Plugin must show a 'Welcome back' message for return visits"
    );
}

// ---------------------------------------------------------------------------
// Memory pattern matches Rust sdk-example-plugin
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_memory_pattern_matches_rust() {
    let src = plugin_source();

    // Rust plugin does: memory::recall(&key)? / memory::store(&key, &val)?
    // Python plugin should do: memory.recall(key) / memory.store(key, val)
    assert!(
        src.contains("memory.recall(") && src.contains("memory.store("),
        "Python plugin must use memory.recall() and memory.store(), \
         matching the Rust sdk-example-plugin pattern"
    );

    // Both plugins use the recalled value to determine first_visit
    assert!(
        src.contains("first_visit"),
        "Python plugin must derive first_visit from memory.recall() result, \
         matching the Rust sdk-example-plugin flow"
    );
}
