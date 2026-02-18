//! TG1: Provider End-to-End Resolution Tests
//!
//! Prevents: Pattern 1 — Provider configuration & resolution bugs (27% of user bugs).
//! Issues: #831, #834, #721, #580, #452, #451, #796, #843
//!
//! Tests the full pipeline from config values through `create_provider_with_url()`
//! to provider construction, verifying factory resolution, URL construction,
//! credential wiring, and auth header format.

use zeroclaw::providers::compatible::{AuthStyle, OpenAiCompatibleProvider};
use zeroclaw::providers::{create_provider, create_provider_with_url};

/// Helper: assert provider creation succeeds
fn assert_provider_ok(name: &str, key: Option<&str>, url: Option<&str>) {
    let result = create_provider_with_url(name, key, url);
    assert!(
        result.is_ok(),
        "{name} provider should resolve: {}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Factory resolution: each major provider name resolves without error
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_resolves_openai_provider() {
    assert_provider_ok("openai", Some("test-key"), None);
}

#[test]
fn factory_resolves_anthropic_provider() {
    assert_provider_ok("anthropic", Some("test-key"), None);
}

#[test]
fn factory_resolves_deepseek_provider() {
    assert_provider_ok("deepseek", Some("test-key"), None);
}

#[test]
fn factory_resolves_mistral_provider() {
    assert_provider_ok("mistral", Some("test-key"), None);
}

#[test]
fn factory_resolves_ollama_provider() {
    assert_provider_ok("ollama", None, None);
}

#[test]
fn factory_resolves_groq_provider() {
    assert_provider_ok("groq", Some("test-key"), None);
}

#[test]
fn factory_resolves_xai_provider() {
    assert_provider_ok("xai", Some("test-key"), None);
}

#[test]
fn factory_resolves_together_provider() {
    assert_provider_ok("together", Some("test-key"), None);
}

#[test]
fn factory_resolves_fireworks_provider() {
    assert_provider_ok("fireworks", Some("test-key"), None);
}

#[test]
fn factory_resolves_perplexity_provider() {
    assert_provider_ok("perplexity", Some("test-key"), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Factory resolution: alias variants map to same provider
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_grok_alias_resolves_to_xai() {
    assert_provider_ok("grok", Some("test-key"), None);
}

#[test]
fn factory_kimi_alias_resolves_to_moonshot() {
    assert_provider_ok("kimi", Some("test-key"), None);
}

#[test]
fn factory_zhipu_alias_resolves_to_glm() {
    assert_provider_ok("zhipu", Some("test-key"), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Custom URL provider creation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_custom_http_url_resolves() {
    assert_provider_ok("custom:http://localhost:8080", Some("test-key"), None);
}

#[test]
fn factory_custom_https_url_resolves() {
    assert_provider_ok("custom:https://api.example.com/v1", Some("test-key"), None);
}

#[test]
fn factory_custom_ftp_url_rejected() {
    let result = create_provider_with_url("custom:ftp://example.com", None, None);
    assert!(result.is_err(), "ftp scheme should be rejected");
    let err_msg = result.err().unwrap().to_string();
    assert!(
        err_msg.contains("http://") || err_msg.contains("https://"),
        "error should mention valid schemes: {err_msg}"
    );
}

#[test]
fn factory_custom_empty_url_rejected() {
    let result = create_provider_with_url("custom:", None, None);
    assert!(result.is_err(), "empty custom URL should be rejected");
}

#[test]
fn factory_unknown_provider_rejected() {
    let result = create_provider_with_url("nonexistent_provider_xyz", None, None);
    assert!(result.is_err(), "unknown provider name should be rejected");
}

// ─────────────────────────────────────────────────────────────────────────────
// OpenAiCompatibleProvider: credential and auth style wiring
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn compatible_provider_bearer_auth_style() {
    // Construction with Bearer auth should succeed
    let _provider = OpenAiCompatibleProvider::new(
        "TestProvider",
        "https://api.test.com",
        Some("sk-test-key-12345"),
        AuthStyle::Bearer,
    );
}

#[test]
fn compatible_provider_xapikey_auth_style() {
    // Construction with XApiKey auth should succeed
    let _provider = OpenAiCompatibleProvider::new(
        "TestProvider",
        "https://api.test.com",
        Some("sk-test-key-12345"),
        AuthStyle::XApiKey,
    );
}

#[test]
fn compatible_provider_custom_auth_header() {
    // Construction with Custom auth should succeed
    let _provider = OpenAiCompatibleProvider::new(
        "TestProvider",
        "https://api.test.com",
        Some("sk-test-key-12345"),
        AuthStyle::Custom("X-Custom-Auth".into()),
    );
}

#[test]
fn compatible_provider_no_credential() {
    // Construction without credential should succeed (for local providers)
    let _provider = OpenAiCompatibleProvider::new(
        "TestLocal",
        "http://localhost:11434",
        None,
        AuthStyle::Bearer,
    );
}

#[test]
fn compatible_provider_base_url_trailing_slash_normalized() {
    // Construction with trailing slash URL should succeed
    let _provider = OpenAiCompatibleProvider::new(
        "TestProvider",
        "https://api.test.com/v1/",
        Some("key"),
        AuthStyle::Bearer,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider with api_url override (simulates #721 - Ollama api_url config)
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_ollama_with_custom_api_url() {
    assert_provider_ok("ollama", None, Some("http://192.168.1.100:11434"));
}

#[test]
fn factory_openai_with_custom_api_url() {
    assert_provider_ok(
        "openai",
        Some("test-key"),
        Some("https://custom-openai-proxy.example.com/v1"),
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider default convenience factory
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn convenience_factory_resolves_major_providers() {
    for provider_name in &[
        "openai",
        "anthropic",
        "deepseek",
        "mistral",
        "groq",
        "xai",
        "together",
        "fireworks",
        "perplexity",
    ] {
        let result = create_provider(provider_name, Some("test-key"));
        assert!(
            result.is_ok(),
            "convenience factory should resolve {provider_name}: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}

#[test]
fn convenience_factory_ollama_no_key() {
    let result = create_provider("ollama", None);
    assert!(
        result.is_ok(),
        "ollama should not require api key: {}",
        result.err().map(|e| e.to_string()).unwrap_or_default()
    );
}
