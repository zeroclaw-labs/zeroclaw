#![cfg(feature = "plugins-wasm")]

//! Integration test: plugin tool calls count against max_actions_per_hour rate limit.
//!
//! Verifies the acceptance criterion for US-ZCL-17:
//! > Plugin tool calls count against max_actions_per_hour rate limit
//!
//! Creates a WasmTool backed by a real WASM plugin, attaches a SecurityPolicy
//! with a low action budget, and verifies that each successful call decrements
//! the budget until the rate limit blocks further calls.

use std::sync::{Arc, Mutex};

use serde_json::json;

use zeroclaw::plugins::wasm_tool::WasmTool;
use zeroclaw::security::policy::SecurityPolicy;
use zeroclaw::tools::traits::Tool;

/// Build a minimal WASM plugin that has no exports (calls will fail, but
/// we only care that the rate limiter records each attempt).
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
        "rate_test_tool".to_string(),
        "test tool for rate limiting".to_string(),
        "rate-test".to_string(),
        "0.1.0".to_string(),
        "some_export".to_string(),
        json!({"type": "object"}),
        make_test_plugin(),
    )
    .with_security_policy(policy)
}

#[tokio::test]
async fn plugin_calls_count_against_rate_limit() {
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 3,
        ..SecurityPolicy::default()
    });

    let tool = make_wasm_tool(Arc::clone(&policy));

    // First 3 calls should be allowed (they'll fail at the WASM level because
    // there's no export, but the rate limiter should still record each one).
    for i in 0..3 {
        let result = tool
            .execute(json!({}))
            .await
            .expect("execute should return Ok");
        // The call fails because the minimal WASM has no exports, but it was
        // NOT blocked by the rate limiter — the error comes from Extism.
        assert!(
            result
                .error
                .as_deref()
                .map_or(true, |e| !e.contains("Rate limit")),
            "call {} should not be rate-limited, got: {:?}",
            i,
            result.error
        );
    }

    // 4th call should be blocked by the rate limiter before reaching WASM.
    let result = tool
        .execute(json!({}))
        .await
        .expect("execute should return Ok");
    assert!(!result.success, "4th call must fail");
    let err = result.error.as_deref().expect("error must be set");
    assert!(
        err.contains("Rate limit"),
        "4th call should be rate-limited, got: {}",
        err
    );
}

#[tokio::test]
async fn rate_limit_shared_across_plugin_tools() {
    // Two different WasmTool instances sharing the same SecurityPolicy should
    // draw from the same action budget.
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 2,
        ..SecurityPolicy::default()
    });

    let tool_a = make_wasm_tool(Arc::clone(&policy));
    let tool_b = WasmTool::new(
        "other_tool".to_string(),
        "another test tool".to_string(),
        "rate-test".to_string(),
        "0.1.0".to_string(),
        "other_export".to_string(),
        json!({"type": "object"}),
        make_test_plugin(),
    )
    .with_security_policy(Arc::clone(&policy));

    // tool_a uses 1 action
    let r1 = tool_a.execute(json!({})).await.unwrap();
    assert!(
        r1.error
            .as_deref()
            .map_or(true, |e| !e.contains("Rate limit")),
        "first call should not be rate-limited"
    );

    // tool_b uses 1 action (total: 2, at limit)
    let r2 = tool_b.execute(json!({})).await.unwrap();
    assert!(
        r2.error
            .as_deref()
            .map_or(true, |e| !e.contains("Rate limit")),
        "second call should not be rate-limited"
    );

    // tool_a tries again — budget exhausted
    let r3 = tool_a.execute(json!({})).await.unwrap();
    assert!(!r3.success);
    let err = r3.error.as_deref().expect("error must be set");
    assert!(
        err.contains("Rate limit"),
        "3rd call should be rate-limited, got: {}",
        err
    );
}

#[tokio::test]
async fn rate_limited_response_has_empty_output() {
    let policy = Arc::new(SecurityPolicy {
        max_actions_per_hour: 0,
        ..SecurityPolicy::default()
    });
    let tool = make_wasm_tool(policy);

    let result = tool.execute(json!({})).await.unwrap();
    assert!(!result.success);
    assert!(
        result.output.is_empty(),
        "rate-limited output should be empty"
    );
    assert!(
        result.error.as_deref().unwrap().contains("Rate limit"),
        "error should mention rate limit"
    );
}
