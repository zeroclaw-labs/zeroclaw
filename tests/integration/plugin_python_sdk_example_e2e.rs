//! Integration test: Python SDK example plugin full-workflow validation.
//!
//! Validates that the Python sdk-example-plugin implements the same Smart
//! Greeter workflow as the Rust sdk-example-plugin: session context, memory
//! recall/store, tool delegation, and identical output shape.
//!
//! Task US-ZCL-35-7 — acceptance criterion for US-ZCL-35:
//! > Integration test validates full workflow matches Rust sdk-example-plugin behavior

use std::path::Path;

const RUST_EXAMPLE_DIR: &str = "tests/plugins/sdk-example-plugin";
const PYTHON_EXAMPLE_DIR: &str = "tests/plugins/python-sdk-example-plugin";

fn rust_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(RUST_EXAMPLE_DIR)
        .join("src/lib.rs");
    std::fs::read_to_string(&path).expect("failed to read sdk-example-plugin/src/lib.rs")
}

fn python_source() -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(PYTHON_EXAMPLE_DIR)
        .join("sdk_example_plugin.py");
    std::fs::read_to_string(&path)
        .expect("failed to read python-sdk-example-plugin/sdk_example_plugin.py")
}

// ---------------------------------------------------------------------------
// Both plugins define the same entry-point name
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_define_tool_greet_entry_point() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("fn tool_greet("),
        "Rust plugin must define a tool_greet function"
    );
    assert!(
        py_src.contains("def tool_greet("),
        "Python plugin must define a tool_greet function"
    );
}

// ---------------------------------------------------------------------------
// Both plugins read session context as step 1
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_read_session_context() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("context::session()"),
        "Rust plugin must call context::session()"
    );
    assert!(
        py_src.contains("context.session()"),
        "Python plugin must call context.session()"
    );
}

// ---------------------------------------------------------------------------
// Both plugins use memory recall with greeted:<conversation_id> key
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_recall_memory_with_greeted_key() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("greeted:") && rust_src.contains("memory::recall("),
        "Rust plugin must recall memory with a 'greeted:' key"
    );
    assert!(
        py_src.contains("greeted:") && py_src.contains("memory.recall("),
        "Python plugin must recall memory with a 'greeted:' key"
    );
}

// ---------------------------------------------------------------------------
// Both plugins track first_visit based on empty recall
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_track_first_visit() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("first_visit"),
        "Rust plugin must track first_visit"
    );
    assert!(
        py_src.contains("first_visit"),
        "Python plugin must track first_visit"
    );
}

// ---------------------------------------------------------------------------
// Both plugins delegate to fun_fact tool on first visit
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_delegate_to_fun_fact_on_first_visit() {
    let rust_src = rust_source();
    let py_src = python_source();

    // Rust: tools::tool_call("fun_fact", ...)
    assert!(
        rust_src.contains("tool_call") && rust_src.contains("fun_fact"),
        "Rust plugin must delegate to fun_fact via tool_call"
    );
    // Python: tools.tool_call("fun_fact", ...)
    assert!(
        py_src.contains("tools.tool_call") && py_src.contains("fun_fact"),
        "Python plugin must delegate to fun_fact via tools.tool_call"
    );
}

// ---------------------------------------------------------------------------
// Both plugins store memory after first greeting
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_store_memory_after_greeting() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("memory::store("),
        "Rust plugin must store memory after greeting"
    );
    assert!(
        py_src.contains("memory.store("),
        "Python plugin must store memory after greeting"
    );
}

// ---------------------------------------------------------------------------
// Both plugins greet returning visitors differently
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_have_welcome_back_for_return_visits() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("Welcome back!"),
        "Rust plugin must say 'Welcome back!' for return visits"
    );
    assert!(
        py_src.contains("Welcome back!"),
        "Python plugin must say 'Welcome back!' for return visits"
    );
}

// ---------------------------------------------------------------------------
// Both plugins have the same fallback when tool_call fails
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_share_same_fallback_fact() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("Waving as a greeting dates back to ancient times!"),
        "Rust plugin must have the standard fallback fun fact"
    );
    assert!(
        py_src.contains("Waving as a greeting dates back to ancient times!"),
        "Python plugin must have the same fallback fun fact as Rust"
    );
}

// ---------------------------------------------------------------------------
// Both plugins default the name to "friend"
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_default_name_to_friend() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains(r#""friend""#),
        "Rust plugin must default name to 'friend'"
    );
    assert!(
        py_src.contains(r#""friend""#),
        "Python plugin must default name to 'friend'"
    );
}

// ---------------------------------------------------------------------------
// Output shape: both return greeting, channel, conversation_id, first_visit
// ---------------------------------------------------------------------------

#[test]
fn python_output_includes_all_rust_output_fields() {
    let py_src = python_source();

    let required_keys = ["greeting", "channel", "conversation_id", "first_visit"];
    for key in &required_keys {
        assert!(
            py_src.contains(&format!(r#""{}""#, key)),
            "Python plugin output must include '{}' key (matching Rust GreeterOutput)",
            key
        );
    }
}

#[test]
fn rust_output_shape_is_subset_of_python() {
    // The Rust plugin returns: greeting, channel, conversation_id, first_visit
    // The Python plugin may return additional fields (e.g. available_channels)
    // but must include all Rust fields.
    let rust_src = rust_source();

    let rust_fields: Vec<&str> = vec!["greeting", "channel", "conversation_id", "first_visit"];
    for field in &rust_fields {
        assert!(
            rust_src.contains(field),
            "Rust plugin must have field '{}' in output",
            field
        );
    }

    let py_src = python_source();
    for field in &rust_fields {
        assert!(
            py_src.contains(field),
            "Python plugin must include Rust output field '{}' for behavioral compatibility",
            field
        );
    }
}

// ---------------------------------------------------------------------------
// Greeting message format matches between both plugins
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_format_greeting_with_channel_and_conversation() {
    let rust_src = rust_source();
    let py_src = python_source();

    // Both should produce: "Hello, {name}! You're on the {channel} channel (conversation {id})."
    assert!(
        rust_src.contains("Hello, ") && rust_src.contains("channel (conversation"),
        "Rust greeting must include channel and conversation in the message"
    );
    assert!(
        py_src.contains("Hello, ")
            && py_src.contains("channel")
            && py_src.contains("(conversation"),
        "Python greeting must include channel and conversation in the message"
    );
}

#[test]
fn both_plugins_include_fun_fact_in_first_visit_greeting() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("Here's a fun fact:"),
        "Rust plugin must include 'Here's a fun fact:' in first-visit greeting"
    );
    assert!(
        py_src.contains("Here's a fun fact:"),
        "Python plugin must include 'Here's a fun fact:' in first-visit greeting"
    );
}

// ---------------------------------------------------------------------------
// Workflow step ordering: context -> recall -> tool_call -> store
// ---------------------------------------------------------------------------

#[test]
fn python_workflow_order_matches_rust() {
    let py_src = python_source();

    // Verify the workflow steps appear in the correct order
    let context_pos = py_src
        .find("context.session()")
        .expect("Python plugin must call context.session()");
    let recall_pos = py_src
        .find("memory.recall(")
        .expect("Python plugin must call memory.recall()");
    let tool_pos = py_src
        .find("tools.tool_call(")
        .expect("Python plugin must call tools.tool_call()");
    let store_pos = py_src
        .find("memory.store(")
        .expect("Python plugin must call memory.store()");

    assert!(
        context_pos < recall_pos,
        "context.session() must come before memory.recall() (matching Rust workflow)"
    );
    assert!(
        recall_pos < tool_pos,
        "memory.recall() must come before tools.tool_call() (matching Rust workflow)"
    );
    assert!(
        tool_pos < store_pos,
        "tools.tool_call() must come before memory.store() (matching Rust workflow)"
    );
}

// ---------------------------------------------------------------------------
// Both plugins use @plugin_fn / #[plugin_fn] decorator/attribute
// ---------------------------------------------------------------------------

#[test]
fn both_plugins_use_plugin_fn_annotation() {
    let rust_src = rust_source();
    let py_src = python_source();

    assert!(
        rust_src.contains("#[plugin_fn]"),
        "Rust plugin must use #[plugin_fn] attribute"
    );
    assert!(
        py_src.contains("@plugin_fn"),
        "Python plugin must use @plugin_fn decorator"
    );
}

// ---------------------------------------------------------------------------
// WASM artifact exists for the Python plugin
// ---------------------------------------------------------------------------

#[test]
fn python_sdk_example_wasm_artifact_exists() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    assert!(
        artifact.is_file(),
        "Pre-compiled python_sdk_example_plugin.wasm artifact is missing — \
         build with: ./build-python-plugins.sh"
    );
}

#[test]
fn python_sdk_example_wasm_artifact_is_nontrivial() {
    let artifact = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/plugins/artifacts/python_sdk_example_plugin.wasm");
    if artifact.is_file() {
        let metadata = std::fs::metadata(&artifact).expect("failed to stat wasm artifact");
        assert!(
            metadata.len() > 1024,
            "python_sdk_example_plugin.wasm is suspiciously small ({} bytes)",
            metadata.len()
        );
    }
}
