//! Gemini provider capabilities and contract tests.
//!
//! Validates that the Gemini provider correctly declares its capabilities
//! through the public Provider trait, ensuring the agent loop selects the
//! right tool-calling strategy (prompt-guided, not native).

use zeroclaw::providers::create_provider_with_url;
use zeroclaw::providers::traits::Provider;

fn gemini_provider() -> Box<dyn Provider> {
    create_provider_with_url("gemini", Some("test-key"), None)
        .expect("Gemini provider should resolve with test key")
}

// ─────────────────────────────────────────────────────────────────────────────
// Capabilities declaration
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn gemini_reports_no_native_tool_calling() {
    let provider = gemini_provider();
    let caps = provider.capabilities();
    assert!(
        !caps.native_tool_calling,
        "Gemini should use prompt-guided tool calling, not native"
    );
}

#[test]
fn gemini_reports_vision_support() {
    let provider = gemini_provider();
    let caps = provider.capabilities();
    assert!(caps.vision, "Gemini should report vision support");
}

#[test]
fn gemini_supports_native_tools_returns_false() {
    let provider = gemini_provider();
    assert!(
        !provider.supports_native_tools(),
        "supports_native_tools() must be false to trigger prompt-guided fallback in chat()"
    );
}

#[test]
fn gemini_supports_vision_returns_true() {
    let provider = gemini_provider();
    assert!(provider.supports_vision());
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool conversion contract
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn gemini_convert_tools_returns_prompt_guided() {
    use zeroclaw::providers::traits::ToolsPayload;
    use zeroclaw::tools::ToolSpec;

    let provider = gemini_provider();
    let tools = vec![ToolSpec {
        name: "memory_store".to_string(),
        description: "Store a value in memory".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "key": {"type": "string"},
                "value": {"type": "string"}
            },
            "required": ["key", "value"]
        }),
    }];

    let payload = provider.convert_tools(&tools);
    assert!(
        matches!(payload, ToolsPayload::PromptGuided { .. }),
        "Gemini should return PromptGuided payload since native_tool_calling is false"
    );
}
