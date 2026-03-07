//! Provider subsystem for model inference backends.
//!
//! This module implements the factory pattern for AI model providers. Each provider
//! implements the [`Provider`] trait defined in [`traits`], and is registered in the
//! factory function [`create_provider`] by its canonical string key.
//!
//! The subsystem supports resilient multi-provider configurations through the
//! [`ReliableProvider`](reliable::ReliableProvider) wrapper, which handles fallback
//! chains and automatic retry. Model routing across providers is available via
//! [`create_routed_provider`].
//!
//! # Extension
//!
//! To add a new provider, implement [`Provider`] in a new submodule and register it
//! in [`create_provider_with_url`]. See `AGENTS.md` §7.1 for the full change playbook.

pub mod reliable;
pub mod router;
pub mod traits;

#[allow(unused_imports)]
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, Provider, ProviderCapabilityError,
    ToolCall, ToolResultMessage,
};

use reliable::ReliableProvider;
use std::path::PathBuf;

const MAX_API_ERROR_CHARS: usize = 200;

#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    pub auth_profile_override: Option<String>,
    pub provider_api_url: Option<String>,
    pub zeroclaw_dir: Option<PathBuf>,
    pub secrets_encrypt: bool,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_level: Option<String>,
    pub max_tokens_override: Option<u32>,
    pub model_support_vision: Option<bool>,
}

impl Default for ProviderRuntimeOptions {
    fn default() -> Self {
        Self {
            auth_profile_override: None,
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: true,
            reasoning_enabled: None,
            reasoning_level: None,
            max_tokens_override: None,
            model_support_vision: None,
        }
    }
}

fn is_secret_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':')
}

fn token_end(input: &str, from: usize) -> usize {
    let mut end = from;
    for (i, c) in input[from..].char_indices() {
        if is_secret_char(c) {
            end = from + i + c.len_utf8();
        } else {
            break;
        }
    }
    end
}

/// Scrub known secret-like token prefixes from provider error strings.
///
/// Redacts tokens with prefixes like `sk-`, `xoxb-`, `xoxp-`, `ghp_`, `gho_`,
/// `ghu_`, `github_pat_`, `AIza`, and `AKIA`.
pub fn scrub_secret_patterns(input: &str) -> String {
    const PREFIXES: [(&str, usize); 26] = [
        ("sk-", 1),
        ("xoxb-", 1),
        ("xoxp-", 1),
        ("ghp_", 1),
        ("gho_", 1),
        ("ghu_", 1),
        ("github_pat_", 1),
        ("AIza", 1),
        ("AKIA", 1),
        ("\"access_token\":\"", 8),
        ("\"refresh_token\":\"", 8),
        ("\"id_token\":\"", 8),
        ("\"token\":\"", 8),
        ("\"api_key\":\"", 8),
        ("\"client_secret\":\"", 8),
        ("\"app_secret\":\"", 8),
        ("\"verify_token\":\"", 8),
        ("access_token=", 8),
        ("refresh_token=", 8),
        ("id_token=", 8),
        ("token=", 8),
        ("api_key=", 8),
        ("client_secret=", 8),
        ("app_secret=", 8),
        ("Bearer ", 16),
        ("bearer ", 16),
    ];

    let mut scrubbed = input.to_string();

    for (prefix, min_len) in PREFIXES {
        let mut search_from = 0;
        loop {
            let Some(rel) = scrubbed[search_from..].find(prefix) else {
                break;
            };

            let start = search_from + rel;
            let content_start = start + prefix.len();
            let end = token_end(&scrubbed, content_start);
            let token_len = end.saturating_sub(content_start);

            // Bare prefixes like "sk-" should not stop future scans.
            if token_len < min_len {
                search_from = content_start;
                continue;
            }

            scrubbed.replace_range(start..end, "[REDACTED]");
            search_from = start + "[REDACTED]".len();
        }
    }

    scrubbed
}

/// Sanitize API error text by scrubbing secrets and truncating length.
pub fn sanitize_api_error(input: &str) -> String {
    let scrubbed = scrub_secret_patterns(input);

    if scrubbed.chars().count() <= MAX_API_ERROR_CHARS {
        return scrubbed;
    }

    let mut end = MAX_API_ERROR_CHARS;
    while end > 0 && !scrubbed.is_char_boundary(end) {
        end -= 1;
    }

    format!("{}...", &scrubbed[..end])
}

/// Build a sanitized provider error from a failed HTTP response.
pub async fn api_error(provider: &str, response: reqwest::Response) -> anyhow::Error {
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<failed to read provider error body>".to_string());
    let sanitized = sanitize_api_error(&body);
    anyhow::anyhow!("{provider} API error ({status}): {sanitized}")
}

/// Factory: create the right provider from config (without custom URL)
pub fn create_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_options(name, api_key, &ProviderRuntimeOptions::default())
}

/// Factory: create provider with runtime options (auth profile override, state dir).
pub fn create_provider_with_options(
    name: &str,
    api_key: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_url_and_options(name, api_key, None, options)
}

/// Factory: create the right provider from config with optional custom base URL
pub fn create_provider_with_url(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_url_and_options(name, api_key, api_url, &ProviderRuntimeOptions::default())
}

/// Factory: create provider with optional base URL and runtime options.
///
/// All concrete provider modules have been removed. To add a provider back,
/// implement the `Provider` trait in a new submodule and register it here.
fn create_provider_with_url_and_options(
    name: &str,
    _api_key: Option<&str>,
    _api_url: Option<&str>,
    _options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    anyhow::bail!(
        "Provider '{name}' is not available. All concrete provider modules have been removed.\n\
         To restore a provider, implement the Provider trait and register it in the factory."
    )
}

/// Parse `"provider:profile"` syntax for fallback entries.
///
/// Returns `(provider_name, Some(profile))` when the entry contains a colon-
/// delimited profile, or `(original_str, None)` otherwise.  Entries starting
/// with `custom:` or `anthropic-custom:` are left untouched because the colon
/// is part of the URL scheme.
fn parse_provider_profile(s: &str) -> (&str, Option<&str>) {
    if s.starts_with("custom:") || s.starts_with("anthropic-custom:") {
        return (s, None);
    }
    match s.split_once(':') {
        Some((provider, profile)) if !profile.is_empty() => (provider, Some(profile)),
        _ => (s, None),
    }
}

/// Create provider chain with retry and fallback behavior.
pub fn create_resilient_provider(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
) -> anyhow::Result<Box<dyn Provider>> {
    create_resilient_provider_with_options(
        primary_name,
        api_key,
        api_url,
        reliability,
        &ProviderRuntimeOptions::default(),
    )
}

/// Create provider chain with retry/fallback behavior and auth runtime options.
pub fn create_resilient_provider_with_options(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let mut providers: Vec<(String, Box<dyn Provider>)> = Vec::new();

    let primary_provider =
        create_provider_with_url_and_options(primary_name, api_key, api_url, options)?;
    providers.push((primary_name.to_string(), primary_provider));

    for fallback in &reliability.fallback_providers {
        if fallback == primary_name || providers.iter().any(|(name, _)| name == fallback) {
            continue;
        }

        let (provider_name, profile_override) = parse_provider_profile(fallback);

        let fallback_options = match profile_override {
            Some(profile) => {
                let mut opts = options.clone();
                opts.auth_profile_override = Some(profile.to_string());
                opts
            }
            None => options.clone(),
        };

        match create_provider_with_options(provider_name, None, &fallback_options) {
            Ok(provider) => providers.push((fallback.clone(), provider)),
            Err(_error) => {
                tracing::warn!(
                    fallback_provider = fallback,
                    "Ignoring invalid fallback provider during initialization"
                );
            }
        }
    }

    let reliable = ReliableProvider::new(
        providers,
        reliability.provider_retries,
        reliability.provider_backoff_ms,
    )
    .with_api_keys(reliability.api_keys.clone())
    .with_model_fallbacks(reliability.model_fallbacks.clone())
    .with_vision_override(options.model_support_vision);

    Ok(Box::new(reliable))
}

/// Create a RouterProvider if model routes are configured, otherwise return a
/// standard resilient provider. The router wraps individual providers per route,
/// each with its own retry/fallback chain.
pub fn create_routed_provider(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
    model_routes: &[crate::config::ModelRouteConfig],
    default_model: &str,
) -> anyhow::Result<Box<dyn Provider>> {
    create_routed_provider_with_options(
        primary_name,
        api_key,
        api_url,
        reliability,
        model_routes,
        default_model,
        &ProviderRuntimeOptions::default(),
    )
}

/// Create a routed provider using explicit runtime options.
pub fn create_routed_provider_with_options(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
    model_routes: &[crate::config::ModelRouteConfig],
    default_model: &str,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    if model_routes.is_empty() {
        return create_resilient_provider_with_options(
            primary_name,
            api_key,
            api_url,
            reliability,
            options,
        );
    }

    // Keep a default provider for non-routed model hints.
    let default_provider = create_resilient_provider_with_options(
        primary_name,
        api_key,
        api_url,
        reliability,
        options,
    )?;
    let mut providers: Vec<(String, Box<dyn Provider>)> =
        vec![(primary_name.to_string(), default_provider)];

    // Build hint routes with dedicated provider instances so per-route API keys
    // and max_tokens overrides do not bleed across routes.
    for route in model_routes {
        let routed_credential = route.api_key.as_ref().and_then(|raw_key| {
            let trimmed_key = raw_key.trim();
            (!trimmed_key.is_empty()).then_some(trimmed_key)
        });
        let key = routed_credential.or(api_key);
        // Only use api_url for routes targeting the same provider namespace.
        let url = (route.provider == primary_name)
            .then_some(api_url)
            .flatten();

        let route_options = options.clone();

        match create_resilient_provider_with_options(
            &route.provider,
            key,
            url,
            reliability,
            &route_options,
        ) {
            Ok(provider) => {
                let provider_id = format!("{}#{}", route.provider, route.hint);
                providers.push((provider_id, provider));
            }
            Err(error) => {
                tracing::warn!(
                    provider = route.provider.as_str(),
                    hint = route.hint.as_str(),
                    "Ignoring routed provider that failed to initialize: {error}"
                );
            }
        }
    }

    // Build route table
    let routes: Vec<(String, router::Route)> = model_routes
        .iter()
        .map(|r| {
            (
                r.hint.clone(),
                router::Route {
                    provider_name: r.provider.clone(),
                    model: r.model.clone(),
                },
            )
        })
        .collect();

    Ok(Box::new(
        router::RouterProvider::new(providers, routes, default_model.to_string())
            .with_vision_override(options.model_support_vision),
    ))
}

/// Information about a supported provider for display purposes.
pub struct ProviderInfo {
    /// Canonical name used in config (e.g. `"openrouter"`)
    pub name: &'static str,
    /// Human-readable display name
    pub display_name: &'static str,
    /// Alternative names accepted in config
    pub aliases: &'static [&'static str],
    /// Whether the provider runs locally (no API key required)
    pub local: bool,
}

/// Return the list of all known providers for display in `zeroclaw providers list`.
///
/// All concrete provider modules have been removed. This list is currently empty.
/// Re-populate as providers are re-added.
pub fn list_providers() -> Vec<ProviderInfo> {
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Factory error cases ──────────────────────────────────

    #[test]
    fn factory_unknown_provider_errors() {
        let p = create_provider("nonexistent", None);
        assert!(p.is_err());
        let msg = p.err().unwrap().to_string();
        assert!(msg.contains("not available"));
    }

    #[test]
    fn factory_empty_name_errors() {
        assert!(create_provider("", None).is_err());
    }

    #[test]
    fn factory_any_name_errors_since_no_providers() {
        // All concrete providers have been removed; every name should fail.
        for name in ["openai", "anthropic", "ollama", "gemini", "openrouter"] {
            assert!(
                create_provider(name, Some("test-key")).is_err(),
                "Provider '{name}' should not be available"
            );
        }
    }

    // ── parse_provider_profile ───────────────────────────────

    #[test]
    fn parse_provider_profile_plain_name() {
        let (name, profile) = parse_provider_profile("gemini");
        assert_eq!(name, "gemini");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_with_profile() {
        let (name, profile) = parse_provider_profile("openai-codex:second");
        assert_eq!(name, "openai-codex");
        assert_eq!(profile, Some("second"));
    }

    #[test]
    fn parse_provider_profile_custom_url_not_split() {
        let input = "custom:https://my-api.example.com/v1";
        let (name, profile) = parse_provider_profile(input);
        assert_eq!(name, input);
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_anthropic_custom_not_split() {
        let input = "anthropic-custom:https://bedrock.example.com";
        let (name, profile) = parse_provider_profile(input);
        assert_eq!(name, input);
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_empty_profile_ignored() {
        let (name, profile) = parse_provider_profile("openai-codex:");
        assert_eq!(name, "openai-codex:");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_extra_colons_kept() {
        let (name, profile) = parse_provider_profile("provider:profile:extra");
        assert_eq!(name, "provider");
        assert_eq!(profile, Some("profile:extra"));
    }

    // ── API error sanitization ───────────────────────────────

    #[test]
    fn sanitize_scrubs_sk_prefix() {
        let input = "request failed: sk-1234567890abcdef";
        let out = sanitize_api_error(input);
        assert!(!out.contains("sk-1234567890abcdef"));
        assert!(out.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_scrubs_multiple_prefixes() {
        let input = "keys sk-abcdef xoxb-12345 xoxp-67890";
        let out = sanitize_api_error(input);
        assert!(!out.contains("sk-abcdef"));
        assert!(!out.contains("xoxb-12345"));
        assert!(!out.contains("xoxp-67890"));
    }

    #[test]
    fn sanitize_short_prefix_then_real_key() {
        let input = "error with sk- prefix and key sk-1234567890";
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-1234567890"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_sk_proj_comment_then_real_key() {
        let input = "note: sk- then sk-proj-abc123def456";
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-proj-abc123def456"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_keeps_bare_prefix() {
        let input = "only prefix sk- present";
        let result = sanitize_api_error(input);
        assert!(result.contains("sk-"));
    }

    #[test]
    fn sanitize_handles_json_wrapped_key() {
        let input = r#"{"error":"invalid key sk-abc123xyz"}"#;
        let result = sanitize_api_error(input);
        assert!(!result.contains("sk-abc123xyz"));
    }

    #[test]
    fn sanitize_handles_delimiter_boundaries() {
        let input = "bad token xoxb-abc123}; next";
        let result = sanitize_api_error(input);
        assert!(!result.contains("xoxb-abc123"));
        assert!(result.contains("};"));
    }

    #[test]
    fn sanitize_truncates_long_error() {
        let long = "a".repeat(400);
        let result = sanitize_api_error(&long);
        assert!(result.len() <= 203);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn sanitize_truncates_after_scrub() {
        let input = format!("{} sk-abcdef123456 {}", "a".repeat(190), "b".repeat(190));
        let result = sanitize_api_error(&input);
        assert!(!result.contains("sk-abcdef123456"));
        assert!(result.len() <= 203);
    }

    #[test]
    fn sanitize_preserves_unicode_boundaries() {
        let input = format!("{} sk-abcdef123", "hello🙂".repeat(80));
        let result = sanitize_api_error(&input);
        assert!(std::str::from_utf8(result.as_bytes()).is_ok());
        assert!(!result.contains("sk-abcdef123"));
    }

    #[test]
    fn sanitize_no_secret_no_change() {
        let input = "simple upstream timeout";
        let result = sanitize_api_error(input);
        assert_eq!(result, input);
    }

    #[test]
    fn scrub_github_personal_access_token() {
        let input = "auth failed with token ghp_abc123def456";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "auth failed with token [REDACTED]");
    }

    #[test]
    fn scrub_github_oauth_token() {
        let input = "Bearer gho_1234567890abcdef";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "Bearer [REDACTED]");
    }

    #[test]
    fn scrub_github_user_token() {
        let input = "token ghu_sessiontoken123";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "token [REDACTED]");
    }

    #[test]
    fn scrub_github_fine_grained_pat() {
        let input = "failed: github_pat_11AABBC_xyzzy789";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "failed: [REDACTED]");
    }

    #[test]
    fn scrub_google_api_key_prefix() {
        let input = "upstream returned key AIzaSyA8exampleToken123456";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "upstream returned key [REDACTED]");
    }

    #[test]
    fn scrub_aws_access_key_prefix() {
        let input = "credential leak AKIAIOSFODNN7EXAMPLE";
        let result = scrub_secret_patterns(input);
        assert_eq!(result, "credential leak [REDACTED]");
    }

    #[test]
    fn sanitize_redacts_json_access_token_field() {
        let input = r#"{"access_token":"ya29.a0AfH6SMB1234567890abcdef","error":"invalid"}"#;
        let result = sanitize_api_error(input);
        assert!(!result.contains("ya29.a0AfH6SMB1234567890abcdef"));
        assert!(!result.contains("access_token"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_redacts_query_client_secret_field() {
        let input = "upstream rejected request: client_secret=supersecret1234567890";
        let result = sanitize_api_error(input);
        assert!(!result.contains("supersecret1234567890"));
        assert!(!result.contains("client_secret"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_redacts_json_token_field() {
        let input = r#"{"token":"abcd1234efgh5678","error":"forbidden"}"#;
        let result = sanitize_api_error(input);
        assert!(!result.contains("abcd1234efgh5678"));
        assert!(!result.contains("\"token\""));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_redacts_query_token_field() {
        let input = "request rejected: token=abcd1234efgh5678";
        let result = sanitize_api_error(input);
        assert!(!result.contains("abcd1234efgh5678"));
        assert!(!result.contains("token="));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_redacts_bearer_token_sequence() {
        let input = "authorization failed: Bearer abcdefghijklmnopqrstuvwxyz123456";
        let result = sanitize_api_error(input);
        assert!(!result.contains("abcdefghijklmnopqrstuvwxyz123456"));
        assert!(!result.contains("Bearer abcdefghijklmnopqrstuvwxyz123456"));
        assert!(result.contains("[REDACTED]"));
    }

    #[test]
    fn sanitize_preserves_short_bearer_phrase_without_secret() {
        let input = "Unauthorized — provide Authorization: Bearer token";
        let result = sanitize_api_error(input);
        assert_eq!(result, input);
    }
}
