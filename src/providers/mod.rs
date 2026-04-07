//! Provider subsystem for model inference backends.
//!
//! This module implements the factory pattern for AI model providers. Each provider
//! implements the [`Provider`] trait defined in [`traits`], and is registered in the
//! factory function [`create_provider`] by its canonical string key (e.g., `"openai"`,
//! `"anthropic"`, `"gemini"`). Provider aliases are resolved internally
//! so that user-facing keys remain stable.
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

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod openrouter;
pub mod reliable;
pub mod router;
pub mod traits;

#[allow(unused_imports)]
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, Provider, ProviderCapabilityError,
    ToolCall, ToolResultMessage,
};

use crate::auth::AuthService;
use reliable::ReliableProvider;
use std::path::PathBuf;

const MAX_API_ERROR_CHARS: usize = 500;

#[derive(Debug, Clone)]
pub struct ProviderRuntimeOptions {
    pub auth_profile_override: Option<String>,
    pub provider_api_url: Option<String>,
    pub zeroclaw_dir: Option<PathBuf>,
    pub secrets_encrypt: bool,
    pub reasoning_enabled: Option<bool>,
    pub reasoning_effort: Option<String>,
    /// HTTP request timeout in seconds for LLM provider API calls.
    /// `None` uses the provider's built-in default (120s for compatible providers).
    pub provider_timeout_secs: Option<u64>,
    /// Extra HTTP headers to include in provider API requests.
    /// These are merged from the config file and `ZEROCLAW_EXTRA_HEADERS` env var.
    pub extra_headers: std::collections::HashMap<String, String>,
    /// Custom API path suffix for OpenAI-compatible providers
    /// (e.g. "/v2/generate" instead of the default "/chat/completions").
    pub api_path: Option<String>,
    /// Maximum output tokens for LLM provider API requests.
    /// `None` uses the provider's built-in default.
    pub provider_max_tokens: Option<u32>,
}

impl Default for ProviderRuntimeOptions {
    fn default() -> Self {
        Self {
            auth_profile_override: None,
            provider_api_url: None,
            zeroclaw_dir: None,
            secrets_encrypt: true,
            reasoning_enabled: None,
            reasoning_effort: None,
            provider_timeout_secs: None,
            extra_headers: std::collections::HashMap::new(),
            api_path: None,
            provider_max_tokens: None,
        }
    }
}

pub fn provider_runtime_options_from_config(
    config: &crate::config::Config,
) -> ProviderRuntimeOptions {
    ProviderRuntimeOptions {
        auth_profile_override: None,
        provider_api_url: config.api_url.clone(),
        zeroclaw_dir: config.config_path.parent().map(PathBuf::from),
        secrets_encrypt: config.secrets.encrypt,
        reasoning_enabled: config.runtime.reasoning_enabled,
        reasoning_effort: config.runtime.reasoning_effort.clone(),
        provider_timeout_secs: Some(config.provider_timeout_secs),
        extra_headers: config.extra_headers.clone(),
        api_path: config.api_path.clone(),
        provider_max_tokens: config.provider_max_tokens,
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
/// `ghu_`, and `github_pat_`.
pub fn scrub_secret_patterns(input: &str) -> String {
    const PREFIXES: [&str; 7] = [
        "sk-",
        "xoxb-",
        "xoxp-",
        "ghp_",
        "gho_",
        "ghu_",
        "github_pat_",
    ];

    let mut scrubbed = input.to_string();

    for prefix in PREFIXES {
        let mut search_from = 0;
        loop {
            let Some(rel) = scrubbed[search_from..].find(prefix) else {
                break;
            };

            let start = search_from + rel;
            let content_start = start + prefix.len();
            let end = token_end(&scrubbed, content_start);

            if end == content_start {
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

/// Resolve API key for a provider from config and environment variables.
///
/// Resolution order:
/// 1. Explicitly provided `api_key` parameter (trimmed, filtered if empty)
/// 2. Provider-specific environment variable (e.g., `ANTHROPIC_OAUTH_TOKEN`, `OPENROUTER_API_KEY`)
/// 3. Generic fallback variables (`ZEROCLAW_API_KEY`, `API_KEY`)
fn resolve_provider_credential(name: &str, credential_override: Option<&str>) -> Option<String> {
    if let Some(raw_override) = credential_override {
        let trimmed_override = raw_override.trim();
        if !trimmed_override.is_empty() {
            if name == "anthropic" || name == "openai" {
                let env_candidates: &[&str] = match name {
                    "anthropic" => &["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
                    "openai" => &["OPENAI_API_KEY"],
                    _ => &[],
                };
                for env_var in env_candidates {
                    if let Ok(val) = std::env::var(env_var) {
                        let trimmed = val.trim().to_string();
                        if !trimmed.is_empty() {
                            return Some(trimmed);
                        }
                    }
                }
                return Some(trimmed_override.to_owned());
            }
            return Some(trimmed_override.to_owned());
        }
    }

    let provider_env_candidates: Vec<&str> = match name {
        "anthropic" => vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
        "openrouter" => vec!["OPENROUTER_API_KEY"],
        "openai" => vec!["OPENAI_API_KEY"],
        "gemini" | "google" | "google-gemini" => vec!["GOOGLE_API_KEY", "GEMINI_API_KEY"],
        _ => vec![],
    };

    for env_var in provider_env_candidates {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    for env_var in ["ZEROCLAW_API_KEY", "API_KEY"] {
        if let Ok(value) = std::env::var(env_var) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }

    None
}

/// Check whether an API key's prefix matches the selected provider.
///
/// Returns `Some("likely_provider")` when the key clearly belongs to a
/// *different* provider (cross-provider mismatch).  Returns `None` when
/// everything looks fine or the format is unrecognised.
fn check_api_key_prefix(provider_name: &str, key: &str) -> Option<&'static str> {
    let likely_provider = if key.starts_with("sk-ant-") {
        Some("anthropic")
    } else if key.starts_with("sk-or-") {
        Some("openrouter")
    } else if key.starts_with("sk-") {
        Some("openai")
    } else {
        None
    };

    let expected = likely_provider?;

    let matches = match provider_name {
        "anthropic" => expected == "anthropic",
        "openrouter" => expected == "openrouter",
        "openai" => expected == "openai",
        _ => return None,
    };

    if matches { None } else { Some(expected) }
}

fn parse_custom_provider_url(
    raw_url: &str,
    provider_label: &str,
    format_hint: &str,
) -> anyhow::Result<String> {
    let base_url = raw_url.trim();

    if base_url.is_empty() {
        anyhow::bail!("{provider_label} requires a URL. Format: {format_hint}");
    }

    let parsed = reqwest::Url::parse(base_url).map_err(|_| {
        anyhow::anyhow!("{provider_label} requires a valid URL. Format: {format_hint}")
    })?;

    match parsed.scheme() {
        "http" | "https" => Ok(base_url.to_string()),
        _ => anyhow::bail!(
            "{provider_label} requires an http:// or https:// URL. Format: {format_hint}"
        ),
    }
}

/// Factory: create the right provider from config (without custom URL)
pub fn create_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_options(name, api_key, &ProviderRuntimeOptions::default())
}

pub fn create_provider_with_options(
    name: &str,
    api_key: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let api_url = options
        .provider_api_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    create_provider_with_url_and_options(name, api_key, api_url, options)
}

pub fn create_provider_with_url(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    create_provider_with_url_and_options(name, api_key, api_url, &ProviderRuntimeOptions::default())
}

/// Factory: create provider with optional base URL and runtime options.
fn create_provider_with_url_and_options(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    options: &ProviderRuntimeOptions,
) -> anyhow::Result<Box<dyn Provider>> {
    let resolved_credential = resolve_provider_credential(name, api_key)
        .map(|v| String::from_utf8(v.into_bytes()).unwrap_or_default());
    #[allow(clippy::option_as_ref_deref)]
    let key = resolved_credential.as_ref().map(String::as_str);

    // Pre-flight: catch obvious API-key / provider mismatches early.
    if let Some(key_value) = key {
        let is_custom = name.starts_with("custom:") || name.starts_with("anthropic-custom:");
        let has_custom_url = api_url.map(str::trim).filter(|u| !u.is_empty()).is_some();
        if !is_custom && !has_custom_url {
            if let Some(likely_provider) = check_api_key_prefix(name, key_value) {
                let visible = &key_value[..key_value.len().min(8)];
                anyhow::bail!(
                    "API key prefix mismatch: key \"{visible}...\" looks like a \
                     {likely_provider} key, but provider \"{name}\" is selected. \
                     Set the correct provider-specific env var or use `-p {likely_provider}`."
                );
            }
        }
    }

    match name {
        "openrouter" => Ok(Box::new(
            openrouter::OpenRouterProvider::new(key, options.provider_timeout_secs)
                .with_max_tokens(options.provider_max_tokens),
        )),
        "anthropic" => {
            let mut p = anthropic::AnthropicProvider::new(key);
            if let Some(mt) = options.provider_max_tokens {
                p = p.with_max_tokens(mt);
            }
            Ok(Box::new(p))
        }
        "openai" => {
            let mut p = openai::OpenAiProvider::with_base_url(api_url, key);
            if let Some(mt) = options.provider_max_tokens {
                p = p.with_max_tokens(Some(mt));
            }
            Ok(Box::new(p))
        }
        "gemini" | "google" | "google-gemini" => {
            let state_dir = options.zeroclaw_dir.clone().unwrap_or_else(|| {
                directories::UserDirs::new().map_or_else(
                    || PathBuf::from(".zeroclaw"),
                    |dirs| dirs.home_dir().join(".zeroclaw"),
                )
            });
            let auth_service = AuthService::new(&state_dir, options.secrets_encrypt);
            Ok(Box::new(gemini::GeminiProvider::new_with_auth(
                key,
                auth_service,
                options.auth_profile_override.clone(),
            )))
        }

        // ── Anthropic-compatible custom endpoints ───────────
        name if name.starts_with("anthropic-custom:") => {
            let base_url = parse_custom_provider_url(
                name.strip_prefix("anthropic-custom:").unwrap_or(""),
                "Anthropic-custom provider",
                "anthropic-custom:https://your-api.com",
            )?;
            Ok(Box::new(anthropic::AnthropicProvider::with_base_url(
                key,
                Some(&base_url),
            )))
        }

        _ => anyhow::bail!(
            "Unknown provider: {name}. Supported providers: anthropic, openai, gemini, openrouter.\n\
             Tip: Use \"anthropic-custom:https://your-api.com\" for Anthropic-compatible endpoints."
        ),
    }
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
    .with_model_fallbacks(reliability.model_fallbacks.clone());

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

    let mut needed: Vec<String> = vec![primary_name.to_string()];
    for route in model_routes {
        if !needed.iter().any(|n| n == &route.provider) {
            needed.push(route.provider.clone());
        }
    }

    let mut providers: Vec<(String, Box<dyn Provider>)> = Vec::new();
    for name in &needed {
        let routed_credential = model_routes
            .iter()
            .find(|r| &r.provider == name)
            .and_then(|r| {
                r.api_key.as_ref().and_then(|raw_key| {
                    let trimmed_key = raw_key.trim();
                    (!trimmed_key.is_empty()).then_some(trimmed_key)
                })
            });
        let key = routed_credential.or(api_key);
        let url = if name == primary_name { api_url } else { None };
        match create_resilient_provider_with_options(name, key, url, reliability, options) {
            Ok(provider) => providers.push((name.clone(), provider)),
            Err(e) => {
                if name == primary_name {
                    return Err(e);
                }
                tracing::warn!(
                    provider = name.as_str(),
                    "Ignoring routed provider that failed to initialize"
                );
            }
        }
    }

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

    Ok(Box::new(router::RouterProvider::new(
        providers,
        routes,
        default_model.to_string(),
    )))
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
pub fn list_providers() -> Vec<ProviderInfo> {
    vec![
        ProviderInfo {
            name: "openrouter",
            display_name: "OpenRouter",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "anthropic",
            display_name: "Anthropic",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "openai",
            display_name: "OpenAI",
            aliases: &[],
            local: false,
        },
        ProviderInfo {
            name: "gemini",
            display_name: "Google Gemini",
            aliases: &["google", "google-gemini"],
            local: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: Option<&str>) -> Self {
            let original = std::env::var(key).ok();
            match value {
                Some(next) => unsafe { std::env::set_var(key, next) },
                None => unsafe { std::env::remove_var(key) },
            }

            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(original) = self.original.as_deref() {
                unsafe { std::env::set_var(self.key, original) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
            .lock()
            .expect("env lock poisoned")
    }

    #[test]
    fn resolve_provider_credential_prefers_explicit_argument() {
        let resolved = resolve_provider_credential("openrouter", Some("  explicit-key  "));
        assert_eq!(resolved, Some("explicit-key".to_string()));
    }

    // ── Primary providers ────────────────────────────────────

    #[test]
    fn factory_openrouter() {
        assert!(create_provider("openrouter", Some("provider-test-credential")).is_ok());
        assert!(create_provider("openrouter", None).is_ok());
    }

    #[test]
    fn factory_anthropic() {
        assert!(create_provider("anthropic", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_openai() {
        assert!(create_provider("openai", Some("provider-test-credential")).is_ok());
    }

    #[test]
    fn factory_gemini() {
        assert!(create_provider("gemini", Some("test-key")).is_ok());
        assert!(create_provider("google", Some("test-key")).is_ok());
        assert!(create_provider("google-gemini", Some("test-key")).is_ok());
        assert!(create_provider("gemini", None).is_ok());
    }

    // ── Anthropic-compatible custom endpoints ─────────────────

    #[test]
    fn factory_anthropic_custom_url() {
        let p = create_provider("anthropic-custom:https://api.example.com", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_trailing_slash() {
        let p = create_provider("anthropic-custom:https://api.example.com/", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_no_key() {
        let p = create_provider("anthropic-custom:https://api.example.com", None);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_anthropic_custom_empty_url_errors() {
        match create_provider("anthropic-custom:", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a URL"),
                "Expected 'requires a URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for empty anthropic-custom URL"),
        }
    }

    #[test]
    fn factory_anthropic_custom_invalid_url_errors() {
        match create_provider("anthropic-custom:not-a-url", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a valid URL"),
                "Expected 'requires a valid URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for invalid anthropic-custom URL"),
        }
    }

    #[test]
    fn factory_anthropic_custom_unsupported_scheme_errors() {
        match create_provider("anthropic-custom:ftp://example.com", None) {
            Err(e) => assert!(
                e.to_string().contains("http:// or https://"),
                "Expected scheme validation error, got: {e}"
            ),
            Ok(_) => panic!("Expected error for unsupported anthropic-custom URL scheme"),
        }
    }

    // ── Error cases ──────────────────────────────────────────

    #[test]
    fn factory_unknown_provider_errors() {
        let p = create_provider("nonexistent", None);
        assert!(p.is_err());
        let msg = p.err().unwrap().to_string();
        assert!(msg.contains("Unknown provider"));
        assert!(msg.contains("nonexistent"));
    }

    #[test]
    fn factory_empty_name_errors() {
        assert!(create_provider("", None).is_err());
    }

    #[test]
    fn resilient_provider_ignores_duplicate_and_invalid_fallbacks() {
        let reliability = crate::config::ReliabilityConfig {
            provider_retries: 1,
            provider_backoff_ms: 100,
            fallback_providers: vec![
                "openrouter".into(),
                "nonexistent-provider".into(),
                "openai".into(),
                "openai".into(),
            ],
            api_keys: Vec::new(),
            model_fallbacks: std::collections::HashMap::new(),
            channel_initial_backoff_secs: 2,
            channel_max_backoff_secs: 60,
            scheduler_poll_secs: 15,
            scheduler_retries: 2,
        };

        let provider = create_resilient_provider(
            "openrouter",
            Some("provider-test-credential"),
            None,
            &reliability,
        );
        assert!(provider.is_ok());
    }

    #[test]
    fn resilient_provider_errors_for_invalid_primary() {
        let reliability = crate::config::ReliabilityConfig::default();
        let provider = create_resilient_provider(
            "totally-invalid",
            Some("provider-test-credential"),
            None,
            &reliability,
        );
        assert!(provider.is_err());
    }

    #[test]
    fn factory_all_providers_create_successfully() {
        let providers = ["openrouter", "anthropic", "openai", "gemini"];
        for name in providers {
            assert!(
                create_provider(name, Some("test-key")).is_ok(),
                "Provider '{name}' should create successfully"
            );
        }
    }

    #[test]
    fn listed_providers_have_unique_ids_and_aliases() {
        let providers = list_providers();
        let mut canonical_ids = std::collections::HashSet::new();
        let mut aliases = std::collections::HashSet::new();

        for provider in providers {
            assert!(
                canonical_ids.insert(provider.name),
                "Duplicate canonical provider id: {}",
                provider.name
            );

            for alias in provider.aliases {
                assert_ne!(
                    *alias, provider.name,
                    "Alias must differ from canonical id: {}",
                    provider.name
                );
                assert!(
                    !canonical_ids.contains(alias),
                    "Alias conflicts with canonical provider id: {}",
                    alias
                );
                assert!(aliases.insert(alias), "Duplicate provider alias: {}", alias);
            }
        }
    }

    #[test]
    fn listed_providers_and_aliases_are_constructible() {
        for provider in list_providers() {
            assert!(
                create_provider(provider.name, Some("provider-test-credential")).is_ok(),
                "Canonical provider id should be constructible: {}",
                provider.name
            );

            for alias in provider.aliases {
                assert!(
                    create_provider(alias, Some("provider-test-credential")).is_ok(),
                    "Provider alias should be constructible: {} (for {})",
                    alias,
                    provider.name
                );
            }
        }
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
        let long = "a".repeat(600);
        let result = sanitize_api_error(&long);
        assert!(result.len() <= 503);
        assert!(result.ends_with("..."));
    }

    #[test]
    fn sanitize_truncates_after_scrub() {
        let input = format!("{} sk-abcdef123456 {}", "a".repeat(290), "b".repeat(290));
        let result = sanitize_api_error(&input);
        assert!(!result.contains("sk-abcdef123456"));
        assert!(result.len() <= 503);
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

    // --- parse_provider_profile ---

    #[test]
    fn parse_provider_profile_plain_name() {
        let (name, profile) = parse_provider_profile("gemini");
        assert_eq!(name, "gemini");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_with_profile() {
        let (name, profile) = parse_provider_profile("openai:second");
        assert_eq!(name, "openai");
        assert_eq!(profile, Some("second"));
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
        let (name, profile) = parse_provider_profile("openai:");
        assert_eq!(name, "openai:");
        assert_eq!(profile, None);
    }

    #[test]
    fn parse_provider_profile_extra_colons_kept() {
        let (name, profile) = parse_provider_profile("provider:profile:extra");
        assert_eq!(name, "provider");
        assert_eq!(profile, Some("profile:extra"));
    }

    // ── API key prefix pre-flight ───────────────────────────

    #[test]
    fn api_key_prefix_cross_provider_mismatch() {
        assert_eq!(
            check_api_key_prefix("openrouter", "sk-ant-api03-xyz"),
            Some("anthropic")
        );
        assert_eq!(
            check_api_key_prefix("anthropic", "sk-or-v1-xyz"),
            Some("openrouter")
        );
        assert_eq!(
            check_api_key_prefix("openai", "sk-ant-xyz"),
            Some("anthropic")
        );
    }

    #[test]
    fn api_key_prefix_correct_match() {
        assert_eq!(check_api_key_prefix("anthropic", "sk-ant-api03-xyz"), None);
        assert_eq!(check_api_key_prefix("openrouter", "sk-or-v1-xyz"), None);
        assert_eq!(check_api_key_prefix("openai", "sk-proj-xyz"), None);
    }

    #[test]
    fn api_key_prefix_unknown_provider_skips() {
        assert_eq!(check_api_key_prefix("gemini", "sk-ant-xyz"), None);
    }

    #[test]
    fn api_key_prefix_unknown_key_format_skips() {
        assert_eq!(check_api_key_prefix("openai", "my-custom-key-123"), None);
        assert_eq!(check_api_key_prefix("anthropic", "some-random-key"), None);
    }

    #[test]
    fn provider_runtime_options_default_has_empty_extra_headers() {
        let options = ProviderRuntimeOptions::default();
        assert!(options.extra_headers.is_empty());
    }

    #[test]
    fn provider_runtime_options_extra_headers_passed_through() {
        let mut extra_headers = std::collections::HashMap::new();
        extra_headers.insert("X-Title".to_string(), "zeroclaw".to_string());
        let options = ProviderRuntimeOptions {
            extra_headers,
            ..ProviderRuntimeOptions::default()
        };
        assert_eq!(options.extra_headers.len(), 1);
        assert_eq!(options.extra_headers.get("X-Title").unwrap(), "zeroclaw");
    }
}
