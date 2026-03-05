//! Live E2E tests for quota tools with real auth profiles.
//!
//! These tests require real auth-profiles.json at ~/.zeroclaw/auth-profiles.json
//! Run with: cargo test --test quota_tools_live -- --nocapture
//! Or: cargo test --test quota_tools_live -- --nocapture --ignored (for ignored tests)

use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use zeroclaw::config::Config;
use zeroclaw::tools::quota_tools::{
    CheckProviderQuotaTool, EstimateQuotaCostTool, SwitchProviderTool,
};
use zeroclaw::tools::Tool;

fn zeroclaw_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME not set")).join(".zeroclaw")
}

fn has_auth_profiles() -> bool {
    zeroclaw_dir().join("auth-profiles.json").exists()
}

fn live_config() -> Config {
    Config {
        workspace_dir: zeroclaw_dir(),
        config_path: zeroclaw_dir().join("config.toml"),
        ..Config::default()
    }
}

// ============================================================================
// Test 1: Какие модели доступны?
// ============================================================================
#[tokio::test]
async fn live_check_all_providers_status() {
    if !has_auth_profiles() {
        eprintln!("SKIP: no auth-profiles.json");
        return;
    }

    let tool = CheckProviderQuotaTool::new(Arc::new(live_config()));
    let result = tool.execute(json!({})).await.unwrap();

    println!("\n=== Test: Какие модели доступны? ===");
    println!("{}", result.output);

    assert!(result.success, "Tool execution failed");
    assert!(
        result.output.contains("Quota Status"),
        "Missing 'Quota Status' header"
    );
}

// ============================================================================
// Test 2: Gemini провайдер
// ============================================================================
#[tokio::test]
async fn live_check_gemini_quota() {
    if !has_auth_profiles() {
        eprintln!("SKIP: no auth-profiles.json");
        return;
    }

    let tool = CheckProviderQuotaTool::new(Arc::new(live_config()));
    let result = tool.execute(json!({"provider": "gemini"})).await.unwrap();

    println!("\n=== Test: Gemini Quota ===");
    println!("{}", result.output);

    assert!(result.success, "Tool execution failed");
    assert!(
        result.output.contains("Quota Status"),
        "Missing quota header"
    );
}

// ============================================================================
// Test 3: OpenAI Codex провайдер
// ============================================================================
#[tokio::test]
async fn live_check_openai_codex_quota() {
    if !has_auth_profiles() {
        eprintln!("SKIP: no auth-profiles.json");
        return;
    }

    let tool = CheckProviderQuotaTool::new(Arc::new(live_config()));
    let result = tool
        .execute(json!({"provider": "openai-codex"}))
        .await
        .unwrap();

    println!("\n=== Test: OpenAI Codex Quota ===");
    println!("{}", result.output);

    assert!(result.success, "Tool execution failed");
}

// ============================================================================
// Test 4: Переключение провайдера
// ============================================================================
#[tokio::test]
async fn live_switch_provider() {
    // Use a temp dir so we don't mutate the real config
    let tmp = tempfile::TempDir::new().unwrap();
    let config = Config {
        workspace_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    config.save().await.unwrap();
    let tool = SwitchProviderTool::new(Arc::new(config));

    // Switch to gemini
    let result = tool
        .execute(json!({
            "provider": "gemini",
            "model": "gemini-2.5-flash",
            "reason": "openai-codex rate limited"
        }))
        .await
        .unwrap();

    println!("\n=== Test: Переключение на Gemini ===");
    println!("{}", result.output);

    assert!(result.success);
    assert!(result.output.contains("gemini"));
    assert!(result.output.contains("rate limited"));

    // Switch to openai-codex
    let result = tool
        .execute(json!({
            "provider": "openai-codex",
            "model": "o3-mini",
            "reason": "gemini quota exhausted"
        }))
        .await
        .unwrap();

    println!("\n=== Test: Переключение на OpenAI Codex ===");
    println!("{}", result.output);

    assert!(result.success);
    assert!(result.output.contains("openai-codex"));
}

// ============================================================================
// Test 5: Оценка затрат
// ============================================================================
#[tokio::test]
async fn live_estimate_quota_cost() {
    let tool = EstimateQuotaCostTool;

    let result = tool
        .execute(json!({
            "operation": "chat_response",
            "estimated_tokens": 10000,
            "parallel_count": 3
        }))
        .await
        .unwrap();

    println!("\n=== Test: Оценка затрат (10k tokens x 3) ===");
    println!("{}", result.output);

    assert!(result.success);
    assert!(result.output.contains("30000")); // 10000 * 3
    assert!(result.output.contains("$"));
}

// ============================================================================
// Test 6: Все 3 инструмента зарегистрированы с правильными именами
// ============================================================================
#[test]
fn tools_have_correct_names() {
    let quota_tool = CheckProviderQuotaTool::new(Arc::new(Config::default()));
    let switch_tool = SwitchProviderTool::new(Arc::new(Config::default()));
    let estimate_tool = EstimateQuotaCostTool;

    assert_eq!(quota_tool.name(), "check_provider_quota");
    assert_eq!(switch_tool.name(), "switch_provider");
    assert_eq!(estimate_tool.name(), "estimate_quota_cost");
}

// ============================================================================
// Test 7: Schemas are valid JSON with required fields
// ============================================================================
#[test]
fn tools_have_valid_schemas() {
    let quota_tool = CheckProviderQuotaTool::new(Arc::new(Config::default()));
    let switch_tool = SwitchProviderTool::new(Arc::new(Config::default()));
    let estimate_tool = EstimateQuotaCostTool;

    // All tools should have object schemas with properties
    for (name, schema) in [
        ("check_provider_quota", quota_tool.parameters_schema()),
        ("switch_provider", switch_tool.parameters_schema()),
        ("estimate_quota_cost", estimate_tool.parameters_schema()),
    ] {
        assert!(
            schema["type"] == "object",
            "{name}: schema type should be 'object'"
        );
        assert!(
            schema["properties"].is_object(),
            "{name}: schema should have properties"
        );
    }

    // switch_provider requires "provider"
    let switch_schema = switch_tool.parameters_schema();
    let required = switch_schema["required"].as_array().unwrap();
    assert!(
        required.contains(&json!("provider")),
        "switch_provider should require 'provider'"
    );
}

// ============================================================================
// Test 8: Error parser works with real-world error payloads
// ============================================================================
#[test]
fn error_parser_real_world_payloads() {
    use zeroclaw::providers::error_parser;

    // Real OpenAI Codex usage_limit_reached error
    let payload_1 = r#"{
        "error": {
            "type": "usage_limit_reached",
            "message": "The usage limit has been reached for this organization on o3-mini-2025-01-31 in the current billing period. Upgrade to the next usage tier by adding more funds to your account.",
            "plan_type": "enterprise",
            "resets_at": 1772087057
        }
    }"#;

    let info = error_parser::parse_openai_codex_error(payload_1).unwrap();
    assert_eq!(info.error_type, "usage_limit_reached");
    assert_eq!(info.plan_type, Some("enterprise".to_string()));
    assert!(info.resets_at.is_some());
    let reset_time = info.resets_at.unwrap();
    println!(
        "\n=== Error Parser: resets_at decoded ===\nTimestamp: {}\nHuman: {}",
        reset_time.timestamp(),
        reset_time.format("%Y-%m-%d %H:%M:%S UTC")
    );

    // Real OpenAI rate_limit_exceeded error (without resets_at)
    let payload_2 = r#"{
        "error": {
            "type": "rate_limit_exceeded",
            "message": "Rate limit reached for default-model in organization org-xxx on requests per min (RPM): Limit 3, Used 3, Requested 1. Please try again in 20s."
        }
    }"#;

    let info = error_parser::parse_openai_codex_error(payload_2).unwrap();
    assert_eq!(info.error_type, "rate_limit_exceeded");
    assert!(info.plan_type.is_none());
    assert!(info.resets_at.is_none());
    println!("Rate limit message: {}", info.message);

    // Non-JSON error
    let payload_3 = "Internal Server Error";
    assert!(error_parser::parse_openai_codex_error(payload_3).is_none());
}

// ============================================================================
// Test 9: tool descriptions mention key capabilities
// ============================================================================
#[test]
fn tool_descriptions_mention_key_capabilities() {
    let quota_tool = CheckProviderQuotaTool::new(Arc::new(Config::default()));
    let desc = quota_tool.description();

    // Should mention rate limit checking
    assert!(
        desc.contains("rate limit") || desc.contains("quota"),
        "Description should mention rate limit or quota"
    );

    // Should mention model availability
    assert!(
        desc.contains("available") || desc.contains("model availability"),
        "Description should mention model availability"
    );
}

// ============================================================================
// Test 10: metadata JSON in output is valid
// ============================================================================
#[tokio::test]
async fn output_contains_valid_metadata_json() {
    let tmp = tempfile::TempDir::new().unwrap();
    let config = Config {
        workspace_dir: tmp.path().to_path_buf(),
        config_path: tmp.path().join("config.toml"),
        ..Config::default()
    };
    let tool = CheckProviderQuotaTool::new(Arc::new(config));
    let result = tool.execute(json!({})).await.unwrap();

    // Extract metadata JSON from output
    if let Some(start) = result.output.find("<!-- metadata: ") {
        let json_start = start + "<!-- metadata: ".len();
        if let Some(end) = result.output[json_start..].find(" -->") {
            let json_str = &result.output[json_start..json_start + end];
            let parsed: serde_json::Value =
                serde_json::from_str(json_str).expect("Metadata JSON should be valid");

            println!("\n=== Metadata JSON ===");
            println!("{}", serde_json::to_string_pretty(&parsed).unwrap());

            assert!(parsed["available_providers"].is_array());
            assert!(parsed["rate_limited_providers"].is_array());
            assert!(parsed["circuit_open_providers"].is_array());
        }
    }
}
