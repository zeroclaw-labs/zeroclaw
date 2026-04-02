//! Integration test: rate limit exceeded produces clear error and rejects further calls.
//!
//! Verifies the acceptance criterion for US-ZCL-17:
//! > Rate limit exceeded produces clear error and rejects further calls
//!
//! Focuses on two aspects:
//! 1. The error message is descriptive enough for operators to understand what happened.
//! 2. Once the limit is exceeded, ALL subsequent calls continue to be rejected (not just
//!    the first one after the budget is exhausted).

use std::sync::{Arc, Mutex};

use serde_json::json;

use zeroclaw::plugins::wasm_tool::WasmTool;
use zeroclaw::security::policy::SecurityPolicy;
use zeroclaw::tools::traits::Tool;

fn make_test_plugin() -> Arc<Mutex<extism::Plugin>> {
    let wasm_bytes: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, // \0asm
        0x01, 0x00, 0x00, 0x00, // version 1
    ];
    let manifest = extism::Manifest::new([extism::Wasm::data(wasm_bytes)]);
    let plugin = extism::Plugin::new(&manifest, [], true).expect("minimal wasm should load");
    Arc::new(Mutex::new(plugin))
}

fn make_wasm_tool(policy: Arc<SecurityPolicy>) -> WasmTool {
    WasmTool::new(
        "rate_exceeded_tool".to_string(),
        "test tool for rate limit exceeded behavior".to_string(),
        "rate-exceeded-test".to_string(),
        "0.1.0".to_string(),
        "some_export".to_string(),
        json!({"type": "object"}),
        make_test_plugin(),
    )
    .with_security_policy(policy)
}

#[tokio::test]
async fn rate_limit_error_message_is_clear_and_actionable() {
    // Budget of 0 — every call is immediately rate-limited.
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 0,
        ..SecurityPolicy::default()
    });
    let tool = make_wasm_tool(policy);

    let result = tool.execute(json!({})).await.unwrap();
    assert!(!result.success, "call must fail when budget is 0");

    let err = result.error.as_deref().expect("error field must be set");

    // The error should contain "Rate limit" so the operator/LLM can identify the cause.
    assert!(
        err.contains("Rate limit"),
        "error must mention 'Rate limit', got: {err}"
    );
    // The error should mention what was exhausted so it is actionable.
    assert!(
        err.contains("budget") || err.contains("exceeded"),
        "error must explain what happened (budget/exceeded), got: {err}"
    );
}

#[tokio::test]
async fn subsequent_calls_after_limit_continue_to_be_rejected() {
    // Budget of 1 — one allowed call, then all subsequent must be rejected.
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 1,
        ..SecurityPolicy::default()
    });
    let tool = make_wasm_tool(policy);

    // Use up the single action.
    let first = tool.execute(json!({})).await.unwrap();
    assert!(
        first
            .error
            .as_deref()
            .map_or(true, |e| !e.contains("Rate limit")),
        "first call should not be rate-limited"
    );

    // Verify that multiple subsequent calls are ALL rejected, not just the first.
    for i in 0..5 {
        let result = tool.execute(json!({})).await.unwrap();
        assert!(!result.success, "call {i} after budget exhausted must fail");
        let err = result.error.as_deref().expect("error must be set");
        assert!(
            err.contains("Rate limit"),
            "call {i} after budget exhausted must be rate-limited, got: {err}"
        );
        // Rate-limited calls must not produce output (no partial execution).
        assert!(
            result.output.is_empty(),
            "rate-limited call {i} must have empty output, got: {}",
            result.output
        );
    }
}

#[tokio::test]
async fn rate_limit_error_does_not_leak_internals() {
    // The error message should be operator-friendly, not contain raw stack traces
    // or internal Rust type names.
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 0,
        ..SecurityPolicy::default()
    });
    let tool = make_wasm_tool(policy);

    let result = tool.execute(json!({})).await.unwrap();
    let err = result.error.as_deref().unwrap();

    assert!(
        !err.contains("panicked") && !err.contains("thread '") && !err.contains("RUST_BACKTRACE"),
        "rate limit error should not leak internal details, got: {err}"
    );
}
