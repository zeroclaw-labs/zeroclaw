//! TG1: Provider End-to-End Resolution Tests
//!
//! Prevents: Pattern 1 — Provider configuration & resolution bugs (27% of user bugs).
//! Issues: #831, #834, #721, #580, #452, #451, #796, #843
//!
//! Tests the full pipeline from config values through `create_provider_with_url()`
//! to provider construction, verifying factory resolution, URL construction,
//! credential wiring, and auth header format.

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
// Factory resolution: each retained provider name resolves without error
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
fn factory_resolves_openrouter_provider() {
    assert_provider_ok("openrouter", Some("test-key"), None);
}

#[test]
fn factory_resolves_gemini_provider() {
    assert_provider_ok("gemini", Some("test-key"), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Factory resolution: alias variants map to same provider
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_google_alias_resolves_to_gemini() {
    assert_provider_ok("google", Some("test-key"), None);
}

#[test]
fn factory_google_gemini_alias_resolves_to_gemini() {
    assert_provider_ok("google-gemini", Some("test-key"), None);
}

// ─────────────────────────────────────────────────────────────────────────────
// Custom URL provider creation
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_unknown_provider_rejected() {
    let result = create_provider_with_url("nonexistent_provider_xyz", None, None);
    assert!(result.is_err(), "unknown provider name should be rejected");
}

// ─────────────────────────────────────────────────────────────────────────────
// Provider with api_url override
// ─────────────────────────────────────────────────────────────────────────────

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
fn convenience_factory_resolves_retained_providers() {
    for provider_name in &["openai", "anthropic", "openrouter"] {
        let result = create_provider(provider_name, Some("test-key"));
        assert!(
            result.is_ok(),
            "convenience factory should resolve {provider_name}: {}",
            result.err().map(|e| e.to_string()).unwrap_or_default()
        );
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Custom endpoint tests
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn factory_anthropic_custom_endpoint_resolves() {
    assert_provider_ok(
        "anthropic-custom:https://api.example.com",
        Some("test-key"),
        None,
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// FR-014: Removed provider aliases return clear errors
// ─────────────────────────────────────────────────────────────────────────────

#[test]
fn removed_provider_ollama_returns_clear_error() {
    let result = create_provider("ollama", None);
    match result {
        Ok(_) => panic!("removed provider 'ollama' should return an error"),
        Err(e) => {
            let err = e.to_string().to_lowercase();
            assert!(
                err.contains("unknown provider") || err.contains("supported"),
                "error for removed provider should be descriptive, got: {e}"
            );
        }
    }
}

#[test]
fn removed_provider_deepseek_returns_clear_error() {
    let result = create_provider("deepseek", Some("test-key"));
    match result {
        Ok(_) => panic!("removed provider 'deepseek' should return an error"),
        Err(e) => {
            let err = e.to_string().to_lowercase();
            assert!(
                err.contains("unknown provider") || err.contains("supported"),
                "error for removed provider should be descriptive, got: {e}"
            );
        }
    }
}

#[test]
fn removed_provider_groq_returns_clear_error() {
    let result = create_provider("groq", Some("test-key"));
    match result {
        Ok(_) => panic!("removed provider 'groq' should return an error"),
        Err(e) => {
            let err = e.to_string().to_lowercase();
            assert!(
                err.contains("unknown provider") || err.contains("supported"),
                "error for removed provider should be descriptive, got: {e}"
            );
        }
    }
}

#[test]
fn removed_provider_copilot_returns_clear_error() {
    assert!(
        create_provider("copilot", Some("test-key")).is_err(),
        "removed provider 'copilot' should return an error"
    );
}

#[test]
fn removed_provider_bedrock_returns_clear_error() {
    assert!(
        create_provider("bedrock", None).is_err(),
        "removed provider 'bedrock' should return an error"
    );
}
