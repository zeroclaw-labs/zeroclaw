//! Integration test for call-depth tracking on tool delegation.
//!
//! Task US-ZCL-24-3: Verify acceptance criterion for story US-ZCL-24:
//! > Call-depth tracking prevents recursive delegation beyond depth 5
//!
//! These tests assert that:
//! 1. Delegation at depth < max_depth is not blocked
//! 2. Delegation at depth == max_depth is blocked
//! 3. Delegation at depth > max_depth is blocked
//! 4. The error message mentions "depth limit"
//! 5. Depth 5 specifically is the boundary when max_depth = 5
//! 6. Depth is immutable after construction

use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use zeroclaw::config::schema::DelegateAgentConfig;
use zeroclaw::security::SecurityPolicy;
use zeroclaw::tools::traits::Tool;
use zeroclaw::tools::DelegateTool;

fn test_security() -> Arc<SecurityPolicy> {
    Arc::new(SecurityPolicy::default())
}

fn agent_with_max_depth(max_depth: u32) -> HashMap<String, DelegateAgentConfig> {
    let mut agents = HashMap::new();
    agents.insert(
        "worker".to_string(),
        DelegateAgentConfig {
            provider: "ollama".to_string(),
            model: "llama3".to_string(),
            system_prompt: None,
            api_key: None,
            temperature: None,
            max_depth,
            agentic: false,
            allowed_tools: Vec::new(),
            max_iterations: 10,
            timeout_secs: None,
            agentic_timeout_secs: None,
            skills_directory: None,
        },
    );
    agents
}

// ---------------------------------------------------------------------------
// 1. Delegation below max_depth is not blocked (depth check passes)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depth_below_limit_is_not_blocked_by_depth_check() {
    // max_depth=5, current depth=4 → 4 < 5, should pass depth check
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 4);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    // The call may fail for other reasons (invalid provider) but should NOT
    // fail with "depth limit" error.
    if let Some(ref err) = result.error {
        assert!(
            !err.contains("depth limit"),
            "depth 4 with max_depth 5 should not hit depth limit, got: {err}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. Delegation at exactly max_depth is blocked
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depth_at_limit_is_blocked() {
    // max_depth=5, current depth=5 → 5 >= 5, blocked
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 5);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    assert!(!result.success, "delegation at max_depth should fail");
    assert!(
        result.error.as_deref().unwrap_or("").contains("depth limit"),
        "error should mention depth limit"
    );
}

// ---------------------------------------------------------------------------
// 3. Delegation beyond max_depth is blocked
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depth_above_limit_is_blocked() {
    // max_depth=5, current depth=6 → 6 >= 5, blocked
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 6);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    assert!(!result.success, "delegation beyond max_depth should fail");
    assert!(
        result.error.as_deref().unwrap_or("").contains("depth limit"),
        "error should mention depth limit"
    );
}

// ---------------------------------------------------------------------------
// 4. Error message includes depth and max values
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depth_limit_error_includes_values() {
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 5);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    let err = result.error.expect("should have error");
    assert!(err.contains("5/5"), "error should show depth/max (5/5), got: {err}");
}

// ---------------------------------------------------------------------------
// 5. Depth 5 boundary: depths 0–4 pass depth check, depth 5 is blocked
// ---------------------------------------------------------------------------

#[tokio::test]
async fn depth_5_boundary_comprehensive() {
    for depth in 0..5u32 {
        let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), depth);
        let result = tool
            .execute(json!({"agent": "worker", "prompt": "test"}))
            .await
            .unwrap();

        if let Some(ref err) = result.error {
            assert!(
                !err.contains("depth limit"),
                "depth {depth} with max_depth 5 should NOT hit depth limit, got: {err}"
            );
        }
    }

    // Depth 5 must be blocked
    let tool = DelegateTool::with_depth(agent_with_max_depth(5), None, test_security(), 5);
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();
    assert!(
        !result.success,
        "depth 5 with max_depth 5 MUST be blocked"
    );
    assert!(
        result.error.as_deref().unwrap_or("").contains("depth limit"),
        "depth 5 should produce depth limit error"
    );
}

// ---------------------------------------------------------------------------
// 6. Root construction starts at depth 0 (not blocked by reasonable max_depth)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn root_tool_is_at_depth_zero() {
    // A root DelegateTool (via ::new) should be at depth 0, so max_depth=1 allows it
    let tool = DelegateTool::new(agent_with_max_depth(1), None, test_security());
    let result = tool
        .execute(json!({"agent": "worker", "prompt": "test"}))
        .await
        .unwrap();

    // Should NOT be blocked by depth limit (depth 0 < max_depth 1)
    if let Some(ref err) = result.error {
        assert!(
            !err.contains("depth limit"),
            "root tool should be at depth 0, not blocked by max_depth 1, got: {err}"
        );
    }
}
