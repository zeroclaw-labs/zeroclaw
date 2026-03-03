//! TG7: Provider Schema Conformance Tests
//!
//! Prevents: Pattern 7 — External schema compatibility bugs (7% of user bugs).
//! Issues: #769, #843
//!
//! Tests request/response serialization to verify required fields are present
//! for each provider's API specification. Validates ChatMessage, ChatResponse,
//! ToolCall, and AuthStyle serialization contracts.

use zeroclaw::providers::compatible::AuthStyle;
use zeroclaw::providers::traits::{ChatMessage, ChatResponse, ToolCall};

// ─────────────────────────────────────────────────────────────────────────────
// ChatMessage serialization
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn chat_message_system_role_correct() {
    let msg = ChatMessage::system("You are a helpful assistant");
    assert_eq!(msg.role, "system");
    assert_eq!(msg.content, "You are a helpful assistant");
}

#[test]
fn chat_message_user_role_correct() {
    let msg = ChatMessage::user("Hello");
    assert_eq!(msg.role, "user");
    assert_eq!(msg.content, "Hello");
}

#[test]
fn chat_message_assistant_role_correct() {
    let msg = ChatMessage::assistant("Hi there!");
    assert_eq!(msg.role, "assistant");
    assert_eq!(msg.content, "Hi there!");
}

#[test]
fn chat_message_tool_role_correct() {
    let msg = ChatMessage::tool("tool result");
    assert_eq!(msg.role, "tool");
    assert_eq!(msg.content, "tool result");
}

#[test]
fn chat_message_serializes_to_json_with_required_fields() {
    let msg = ChatMessage::user("test message");
    let json = serde_json::to_value(&msg).unwrap();

    assert!(json.get("role").is_some(), "JSON must have 'role' field");
    assert!(
        json.get("content").is_some(),
        "JSON must have 'content' field"
    );
    assert_eq!(json["role"], "user");
    assert_eq!(json["content"], "test message");
}

#[test]
fn chat_message_json_roundtrip() {
    let original = ChatMessage::assistant("response text");
    let json_str = serde_json::to_string(&original).unwrap();
    let parsed: ChatMessage = serde_json::from_str(&json_str).unwrap();

    assert_eq!(parsed.role, original.role);
    assert_eq!(parsed.content, original.content);
}

// ─────────────────────────────────────────────────────────────────────────────
// ToolCall serialization (#843 - tool_call_id field)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tool_call_has_required_fields() {
    let tc = ToolCall {
        id: "call_abc123".into(),
        name: "web_search".into(),
        arguments: r#"{"query": "rust programming"}"#.into(),
    };

    let json = serde_json::to_value(&tc).unwrap();
    assert!(json.get("id").is_some(), "ToolCall must have 'id' field");
    assert!(
        json.get("name").is_some(),
        "ToolCall must have 'name' field"
    );
    assert!(
        json.get("arguments").is_some(),
        "ToolCall must have 'arguments' field"
    );
}

#[test]
fn tool_call_id_preserved_in_serialization() {
    let tc = ToolCall {
        id: "call_deepseek_42".into(),
        name: "shell".into(),
        arguments: r#"{"command": "ls"}"#.into(),
    };

    let json_str = serde_json::to_string(&tc).unwrap();
    let parsed: ToolCall = serde_json::from_str(&json_str).unwrap();

    assert_eq!(
        parsed.id, "call_deepseek_42",
        "tool_call_id must survive roundtrip"
    );
    assert_eq!(parsed.name, "shell");
}

#[test]
fn tool_call_arguments_contain_valid_json() {
    let tc = ToolCall {
        id: "call_1".into(),
        name: "file_write".into(),
        arguments: r#"{"path": "/tmp/test.txt", "content": "hello"}"#.into(),
    };

    // Arguments should parse as valid JSON
    let args: serde_json::Value =
        serde_json::from_str(&tc.arguments).expect("tool call arguments should be valid JSON");
    assert!(args.get("path").is_some());
    assert!(args.get("content").is_some());
}

// ─────────────────────────────────────────────────────────────────────────────
// Tool message with tool_call_id (DeepSeek requirement)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn tool_response_message_can_embed_tool_call_id() {
    // DeepSeek requires tool_call_id in tool response messages.
    // The tool message content can embed the tool_call_id as JSON.
    let tool_response =
        ChatMessage::tool(r#"{"tool_call_id": "call_abc123", "content": "search results here"}"#);

    let parsed: serde_json::Value = serde_json::from_str(&tool_response.content)
        .expect("tool response content should be valid JSON");

    assert!(
        parsed.get("tool_call_id").is_some(),
        "tool response should include tool_call_id for DeepSeek compatibility"
    );
    assert_eq!(parsed["tool_call_id"], "call_abc123");
}

// ─────────────────────────────────────────────────────────────────────────────
// ChatResponse structure
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn chat_response_text_only() {
    let resp = ChatResponse {
        text: Some("Hello world".into()),
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    assert_eq!(resp.text_or_empty(), "Hello world");
    assert!(!resp.has_tool_calls());
}

#[test]
fn chat_response_with_tool_calls() {
    let resp = ChatResponse {
        text: Some(String::new()),
        tool_calls: vec![ToolCall {
            id: "tc_1".into(),
            name: "echo".into(),
            arguments: "{}".into(),
        }],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    assert!(resp.has_tool_calls());
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].name, "echo");
}

#[test]
fn chat_response_text_or_empty_handles_none() {
    let resp = ChatResponse {
        text: None,
        tool_calls: vec![],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    assert_eq!(resp.text_or_empty(), "");
}

#[test]
fn chat_response_multiple_tool_calls() {
    let resp = ChatResponse {
        text: None,
        tool_calls: vec![
            ToolCall {
                id: "tc_1".into(),
                name: "shell".into(),
                arguments: r#"{"command": "ls"}"#.into(),
            },
            ToolCall {
                id: "tc_2".into(),
                name: "file_read".into(),
                arguments: r#"{"path": "test.txt"}"#.into(),
            },
        ],
        usage: None,
        reasoning_content: None,
        quota_metadata: None,
        stop_reason: None,
        raw_stop_reason: None,
    };

    assert!(resp.has_tool_calls());
    assert_eq!(resp.tool_calls.len(), 2);
    // Each tool call should have a distinct id
    assert_ne!(resp.tool_calls[0].id, resp.tool_calls[1].id);
}

// ─────────────────────────────────────────────────────────────────────────────
// AuthStyle variants
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn auth_style_bearer_is_constructible() {
    let style = AuthStyle::Bearer;
    assert!(matches!(style, AuthStyle::Bearer));
}

#[test]
fn auth_style_xapikey_is_constructible() {
    let style = AuthStyle::XApiKey;
    assert!(matches!(style, AuthStyle::XApiKey));
}

#[test]
fn auth_style_custom_header() {
    let style = AuthStyle::Custom("X-Custom-Auth".into());
    if let AuthStyle::Custom(header) = style {
        assert_eq!(header, "X-Custom-Auth");
    } else {
        panic!("expected AuthStyle::Custom");
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider naming consistency
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn provider_construction_with_different_names() {
    use zeroclaw::providers::compatible::OpenAiCompatibleProvider;

    // Construction with various names should succeed
    let _p1 = OpenAiCompatibleProvider::new(
        "DeepSeek",
        "https://api.deepseek.com",
        Some("test-key"),
        AuthStyle::Bearer,
    );
    let _p2 =
        OpenAiCompatibleProvider::new("deepseek", "https://api.test.com", None, AuthStyle::Bearer);
}

#[test]
fn provider_construction_with_different_auth_styles() {
    use zeroclaw::providers::compatible::OpenAiCompatibleProvider;

    let _bearer = OpenAiCompatibleProvider::new(
        "Test",
        "https://api.test.com",
        Some("key"),
        AuthStyle::Bearer,
    );
    let _xapi = OpenAiCompatibleProvider::new(
        "Test",
        "https://api.test.com",
        Some("key"),
        AuthStyle::XApiKey,
    );
    let _custom = OpenAiCompatibleProvider::new(
        "Test",
        "https://api.test.com",
        Some("key"),
        AuthStyle::Custom("X-My-Auth".into()),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Conversation history message ordering
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn chat_messages_maintain_role_sequence() {
    let history = [
        ChatMessage::system("You are helpful"),
        ChatMessage::user("What is Rust?"),
        ChatMessage::assistant("Rust is a systems programming language"),
        ChatMessage::user("Tell me more"),
        ChatMessage::assistant("It emphasizes safety and performance"),
    ];

    assert_eq!(history[0].role, "system");
    assert_eq!(history[1].role, "user");
    assert_eq!(history[2].role, "assistant");
    assert_eq!(history[3].role, "user");
    assert_eq!(history[4].role, "assistant");
}

#[test]
fn chat_messages_with_tool_calls_maintain_sequence() {
    let history = [
        ChatMessage::system("You are helpful"),
        ChatMessage::user("Search for Rust"),
        ChatMessage::assistant("I'll search for that"),
        ChatMessage::tool(r#"{"tool_call_id": "tc_1", "content": "search results"}"#),
        ChatMessage::assistant("Based on the search results..."),
    ];

    assert_eq!(history.len(), 5);
    assert_eq!(history[3].role, "tool");
    assert_eq!(history[4].role, "assistant");

    // Verify tool message content is valid JSON with tool_call_id
    let tool_content: serde_json::Value = serde_json::from_str(&history[3].content).unwrap();
    assert!(tool_content.get("tool_call_id").is_some());
}
