pub mod anthropic;
pub mod compatible;
pub mod copilot;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openrouter;
pub mod reliable;
pub mod router;
pub mod traits;

#[allow(unused_imports)]
pub use traits::{
    ChatMessage, ChatRequest, ChatResponse, ConversationMessage, Provider, ToolCall,
    ToolResultMessage,
};

use compatible::{AuthStyle, OpenAiCompatibleProvider};
use reliable::ReliableProvider;

const MAX_API_ERROR_CHARS: usize = 200;
const MINIMAX_INTL_BASE_URL: &str = "https://api.minimax.io/v1";
const MINIMAX_CN_BASE_URL: &str = "https://api.minimaxi.com/v1";
const GLM_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/paas/v4";
const GLM_CN_BASE_URL: &str = "https://open.bigmodel.cn/api/paas/v4";
const MOONSHOT_INTL_BASE_URL: &str = "https://api.moonshot.ai/v1";
const MOONSHOT_CN_BASE_URL: &str = "https://api.moonshot.cn/v1";
const QWEN_CN_BASE_URL: &str = "https://dashscope.aliyuncs.com/compatible-mode/v1";
const QWEN_INTL_BASE_URL: &str = "https://dashscope-intl.aliyuncs.com/compatible-mode/v1";
const QWEN_US_BASE_URL: &str = "https://dashscope-us.aliyuncs.com/compatible-mode/v1";
const ZAI_GLOBAL_BASE_URL: &str = "https://api.z.ai/api/coding/paas/v4";
const ZAI_CN_BASE_URL: &str = "https://open.bigmodel.cn/api/coding/paas/v4";

fn minimax_base_url(name: &str) -> Option<&'static str> {
    match name {
        "minimax" | "minimax-intl" | "minimax-io" | "minimax-global" => Some(MINIMAX_INTL_BASE_URL),
        "minimax-cn" | "minimaxi" => Some(MINIMAX_CN_BASE_URL),
        _ => None,
    }
}

fn glm_base_url(name: &str) -> Option<&'static str> {
    match name {
        "glm" | "zhipu" | "glm-global" | "zhipu-global" => Some(GLM_GLOBAL_BASE_URL),
        "glm-cn" | "zhipu-cn" | "bigmodel" => Some(GLM_CN_BASE_URL),
        _ => None,
    }
}

fn moonshot_base_url(name: &str) -> Option<&'static str> {
    match name {
        "moonshot-intl" | "moonshot-global" | "kimi-intl" | "kimi-global" => {
            Some(MOONSHOT_INTL_BASE_URL)
        }
        "moonshot" | "kimi" | "moonshot-cn" | "kimi-cn" => Some(MOONSHOT_CN_BASE_URL),
        _ => None,
    }
}

fn qwen_base_url(name: &str) -> Option<&'static str> {
    match name {
        "qwen" | "dashscope" | "qwen-cn" | "dashscope-cn" => Some(QWEN_CN_BASE_URL),
        "qwen-intl" | "dashscope-intl" | "qwen-international" | "dashscope-international" => {
            Some(QWEN_INTL_BASE_URL)
        }
        "qwen-us" | "dashscope-us" => Some(QWEN_US_BASE_URL),
        _ => None,
    }
}

fn zai_base_url(name: &str) -> Option<&'static str> {
    match name {
        "zai" | "z.ai" | "zai-global" | "z.ai-global" => Some(ZAI_GLOBAL_BASE_URL),
        "zai-cn" | "z.ai-cn" => Some(ZAI_CN_BASE_URL),
        _ => None,
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

            // Bare prefixes like "sk-" should not stop future scans.
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
///
/// For Anthropic, the provider-specific env var is `ANTHROPIC_OAUTH_TOKEN` (for setup-tokens)
/// followed by `ANTHROPIC_API_KEY` (for regular API keys).
fn resolve_provider_credential(name: &str, credential_override: Option<&str>) -> Option<String> {
    if let Some(raw_override) = credential_override {
        let trimmed_override = raw_override.trim();
        if !trimmed_override.is_empty() {
            return Some(trimmed_override.to_owned());
        }
    }

    let provider_env_candidates: Vec<&str> = match name {
        "anthropic" => vec!["ANTHROPIC_OAUTH_TOKEN", "ANTHROPIC_API_KEY"],
        "openrouter" => vec!["OPENROUTER_API_KEY"],
        "openai" => vec!["OPENAI_API_KEY"],
        "ollama" => vec!["OLLAMA_API_KEY"],
        "venice" => vec!["VENICE_API_KEY"],
        "groq" => vec!["GROQ_API_KEY"],
        "mistral" => vec!["MISTRAL_API_KEY"],
        "deepseek" => vec!["DEEPSEEK_API_KEY"],
        "xai" | "grok" => vec!["XAI_API_KEY"],
        "together" | "together-ai" => vec!["TOGETHER_API_KEY"],
        "fireworks" | "fireworks-ai" => vec!["FIREWORKS_API_KEY"],
        "perplexity" => vec!["PERPLEXITY_API_KEY"],
        "cohere" => vec!["COHERE_API_KEY"],
        "moonshot" | "kimi" | "moonshot-intl" | "moonshot-global" | "moonshot-cn" | "kimi-intl"
        | "kimi-global" | "kimi-cn" => vec!["MOONSHOT_API_KEY"],
        "glm" | "zhipu" | "glm-global" | "zhipu-global" | "glm-cn" | "zhipu-cn" | "bigmodel" => {
            vec!["GLM_API_KEY"]
        }
        "minimax" | "minimax-intl" | "minimax-io" | "minimax-global" | "minimax-cn"
        | "minimaxi" => vec!["MINIMAX_API_KEY"],
        "qianfan" | "baidu" => vec!["QIANFAN_API_KEY"],
        "qwen"
        | "dashscope"
        | "qwen-cn"
        | "dashscope-cn"
        | "qwen-intl"
        | "dashscope-intl"
        | "qwen-international"
        | "dashscope-international"
        | "qwen-us"
        | "dashscope-us" => vec!["DASHSCOPE_API_KEY"],
        "zai" | "z.ai" | "zai-global" | "z.ai-global" | "zai-cn" | "z.ai-cn" => {
            vec!["ZAI_API_KEY"]
        }
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => vec!["NVIDIA_API_KEY"],
        "synthetic" => vec!["SYNTHETIC_API_KEY"],
        "opencode" | "opencode-zen" => vec!["OPENCODE_API_KEY"],
        "vercel" | "vercel-ai" => vec!["VERCEL_API_KEY"],
        "cloudflare" | "cloudflare-ai" => vec!["CLOUDFLARE_API_KEY"],
        "astrai" => vec!["ASTRAI_API_KEY"],
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
    create_provider_with_url(name, api_key, None)
}

/// Factory: create the right provider from config with optional custom base URL
#[allow(clippy::too_many_lines)]
pub fn create_provider_with_url(
    name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
) -> anyhow::Result<Box<dyn Provider>> {
    let resolved_credential = resolve_provider_credential(name, api_key);
    #[allow(clippy::option_as_ref_deref)]
    let key = resolved_credential.as_ref().map(String::as_str);
    match name {
        // ── Primary providers (custom implementations) ───────
        "openrouter" => Ok(Box::new(openrouter::OpenRouterProvider::new(key))),
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider::new(key))),
        "openai" => Ok(Box::new(openai::OpenAiProvider::new(key))),
        // Ollama uses api_url for custom base URL (e.g. remote Ollama instance)
        "ollama" => Ok(Box::new(ollama::OllamaProvider::new(api_url, key))),
        "gemini" | "google" | "google-gemini" => {
            Ok(Box::new(gemini::GeminiProvider::new(key)))
        }

        // ── OpenAI-compatible providers ──────────────────────
        "venice" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Venice", "https://api.venice.ai", key, AuthStyle::Bearer,
        ))),
        "vercel" | "vercel-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Vercel AI Gateway", "https://api.vercel.ai", key, AuthStyle::Bearer,
        ))),
        "cloudflare" | "cloudflare-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Cloudflare AI Gateway",
            "https://gateway.ai.cloudflare.com/v1",
            key,
            AuthStyle::Bearer,
        ))),
        name if moonshot_base_url(name).is_some() => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Moonshot",
            moonshot_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::Bearer,
        ))),
        "synthetic" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Synthetic", "https://api.synthetic.com", key, AuthStyle::Bearer,
        ))),
        "opencode" | "opencode-zen" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "OpenCode Zen", "https://opencode.ai/zen/v1", key, AuthStyle::Bearer,
        ))),
        name if zai_base_url(name).is_some() => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Z.AI",
            zai_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::Bearer,
        ))),
        name if glm_base_url(name).is_some() => {
            Ok(Box::new(OpenAiCompatibleProvider::new_no_responses_fallback(
                "GLM",
                glm_base_url(name).expect("checked in guard"),
                key,
                AuthStyle::Bearer,
            )))
        }
        name if minimax_base_url(name).is_some() => Ok(Box::new(OpenAiCompatibleProvider::new(
            "MiniMax",
            minimax_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::Bearer,
        ))),
        "bedrock" | "aws-bedrock" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Amazon Bedrock",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            key,
            AuthStyle::Bearer,
        ))),
        "qianfan" | "baidu" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Qianfan", "https://aip.baidubce.com", key, AuthStyle::Bearer,
        ))),
        name if qwen_base_url(name).is_some() => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Qwen",
            qwen_base_url(name).expect("checked in guard"),
            key,
            AuthStyle::Bearer,
        ))),

        // ── Extended ecosystem (community favorites) ─────────
        "groq" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Groq", "https://api.groq.com/openai", key, AuthStyle::Bearer,
        ))),
        "mistral" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Mistral", "https://api.mistral.ai/v1", key, AuthStyle::Bearer,
        ))),
        "xai" | "grok" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "xAI", "https://api.x.ai", key, AuthStyle::Bearer,
        ))),
        "deepseek" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "DeepSeek", "https://api.deepseek.com", key, AuthStyle::Bearer,
        ))),
        "together" | "together-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Together AI", "https://api.together.xyz", key, AuthStyle::Bearer,
        ))),
        "fireworks" | "fireworks-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Fireworks AI", "https://api.fireworks.ai/inference/v1", key, AuthStyle::Bearer,
        ))),
        "perplexity" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Perplexity", "https://api.perplexity.ai", key, AuthStyle::Bearer,
        ))),
        "cohere" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Cohere", "https://api.cohere.com/compatibility", key, AuthStyle::Bearer,
        ))),
        "copilot" | "github-copilot" => {
            Ok(Box::new(copilot::CopilotProvider::new(api_key)))
        },
        "lmstudio" | "lm-studio" => {
            let lm_studio_key = api_key
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or("lm-studio");
            Ok(Box::new(OpenAiCompatibleProvider::new(
                "LM Studio",
                "http://localhost:1234/v1",
                Some(lm_studio_key),
                AuthStyle::Bearer,
            )))
        }
        "nvidia" | "nvidia-nim" | "build.nvidia.com" => Ok(Box::new(
            OpenAiCompatibleProvider::new(
                "NVIDIA NIM",
                "https://integrate.api.nvidia.com/v1",
                key,
                AuthStyle::Bearer,
            ),
        )),

        // ── AI inference routers ─────────────────────────────
        "astrai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Astrai", "https://as-trai.com/v1", key, AuthStyle::Bearer,
        ))),

        // ── Bring Your Own Provider (custom URL) ───────────
        // Format: "custom:https://your-api.com" or "custom:http://localhost:1234"
        name if name.starts_with("custom:") => {
            let base_url = parse_custom_provider_url(
                name.strip_prefix("custom:").unwrap_or(""),
                "Custom provider",
                "custom:https://your-api.com",
            )?;
            Ok(Box::new(OpenAiCompatibleProvider::new(
                "Custom",
                &base_url,
                key,
                AuthStyle::Bearer,
            )))
        }

        // ── Anthropic-compatible custom endpoints ───────────
        // Format: "anthropic-custom:https://your-api.com"
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
            "Unknown provider: {name}. Check README for supported providers or run `zeroclaw onboard --interactive` to reconfigure.\n\
             Tip: Use \"custom:https://your-api.com\" for OpenAI-compatible endpoints.\n\
             Tip: Use \"anthropic-custom:https://your-api.com\" for Anthropic-compatible endpoints."
        ),
    }
}

/// Create provider chain with retry and fallback behavior.
pub fn create_resilient_provider(
    primary_name: &str,
    api_key: Option<&str>,
    api_url: Option<&str>,
    reliability: &crate::config::ReliabilityConfig,
) -> anyhow::Result<Box<dyn Provider>> {
    let mut providers: Vec<(String, Box<dyn Provider>)> = Vec::new();

    providers.push((
        primary_name.to_string(),
        create_provider_with_url(primary_name, api_key, api_url)?,
    ));

    for fallback in &reliability.fallback_providers {
        if fallback == primary_name || providers.iter().any(|(name, _)| name == fallback) {
            continue;
        }

        // Fallback providers don't use the custom api_url (it's specific to primary)
        match create_provider(fallback, api_key) {
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
    if model_routes.is_empty() {
        return create_resilient_provider(primary_name, api_key, api_url, reliability);
    }

    // Collect unique provider names needed
    let mut needed: Vec<String> = vec![primary_name.to_string()];
    for route in model_routes {
        if !needed.iter().any(|n| n == &route.provider) {
            needed.push(route.provider.clone());
        }
    }

    // Create each provider (with its own resilience wrapper)
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
        // Only use api_url for the primary provider
        let url = if name == primary_name { api_url } else { None };
        match create_resilient_provider(name, key, url, reliability) {
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

    Ok(Box::new(router::RouterProvider::new(
        providers,
        routes,
        default_model.to_string(),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_provider_credential_prefers_explicit_argument() {
        let resolved = resolve_provider_credential("openrouter", Some("  explicit-key  "));
        assert_eq!(resolved, Some("explicit-key".to_string()));
    }

    #[test]
    fn regional_endpoint_aliases_map_to_expected_urls() {
        assert_eq!(minimax_base_url("minimax"), Some(MINIMAX_INTL_BASE_URL));
        assert_eq!(
            minimax_base_url("minimax-intl"),
            Some(MINIMAX_INTL_BASE_URL)
        );
        assert_eq!(minimax_base_url("minimax-cn"), Some(MINIMAX_CN_BASE_URL));

        assert_eq!(glm_base_url("glm"), Some(GLM_GLOBAL_BASE_URL));
        assert_eq!(glm_base_url("glm-cn"), Some(GLM_CN_BASE_URL));
        assert_eq!(glm_base_url("bigmodel"), Some(GLM_CN_BASE_URL));

        assert_eq!(moonshot_base_url("moonshot"), Some(MOONSHOT_CN_BASE_URL));
        assert_eq!(
            moonshot_base_url("moonshot-intl"),
            Some(MOONSHOT_INTL_BASE_URL)
        );

        assert_eq!(qwen_base_url("qwen"), Some(QWEN_CN_BASE_URL));
        assert_eq!(qwen_base_url("qwen-cn"), Some(QWEN_CN_BASE_URL));
        assert_eq!(qwen_base_url("qwen-intl"), Some(QWEN_INTL_BASE_URL));
        assert_eq!(qwen_base_url("qwen-us"), Some(QWEN_US_BASE_URL));

        assert_eq!(zai_base_url("zai"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("z.ai"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("zai-global"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("z.ai-global"), Some(ZAI_GLOBAL_BASE_URL));
        assert_eq!(zai_base_url("zai-cn"), Some(ZAI_CN_BASE_URL));
        assert_eq!(zai_base_url("z.ai-cn"), Some(ZAI_CN_BASE_URL));
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
    fn factory_ollama() {
        assert!(create_provider("ollama", None).is_ok());
        // Ollama may use API key when a remote endpoint is configured.
        assert!(create_provider("ollama", Some("dummy")).is_ok());
        assert!(create_provider("ollama", Some("any-value-here")).is_ok());
    }

    #[test]
    fn factory_gemini() {
        assert!(create_provider("gemini", Some("test-key")).is_ok());
        assert!(create_provider("google", Some("test-key")).is_ok());
        assert!(create_provider("google-gemini", Some("test-key")).is_ok());
        // Should also work without key (will try CLI auth)
        assert!(create_provider("gemini", None).is_ok());
    }

    // ── OpenAI-compatible providers ──────────────────────────

    #[test]
    fn factory_venice() {
        assert!(create_provider("venice", Some("vn-key")).is_ok());
    }

    #[test]
    fn factory_vercel() {
        assert!(create_provider("vercel", Some("key")).is_ok());
        assert!(create_provider("vercel-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_cloudflare() {
        assert!(create_provider("cloudflare", Some("key")).is_ok());
        assert!(create_provider("cloudflare-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_moonshot() {
        assert!(create_provider("moonshot", Some("key")).is_ok());
        assert!(create_provider("kimi", Some("key")).is_ok());
        assert!(create_provider("moonshot-intl", Some("key")).is_ok());
        assert!(create_provider("moonshot-cn", Some("key")).is_ok());
        assert!(create_provider("kimi-intl", Some("key")).is_ok());
        assert!(create_provider("kimi-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_synthetic() {
        assert!(create_provider("synthetic", Some("key")).is_ok());
    }

    #[test]
    fn factory_opencode() {
        assert!(create_provider("opencode", Some("key")).is_ok());
        assert!(create_provider("opencode-zen", Some("key")).is_ok());
    }

    #[test]
    fn factory_zai() {
        assert!(create_provider("zai", Some("key")).is_ok());
        assert!(create_provider("z.ai", Some("key")).is_ok());
        assert!(create_provider("zai-global", Some("key")).is_ok());
        assert!(create_provider("z.ai-global", Some("key")).is_ok());
        assert!(create_provider("zai-cn", Some("key")).is_ok());
        assert!(create_provider("z.ai-cn", Some("key")).is_ok());
    }

    #[test]
    fn factory_glm() {
        assert!(create_provider("glm", Some("key")).is_ok());
        assert!(create_provider("zhipu", Some("key")).is_ok());
        assert!(create_provider("glm-cn", Some("key")).is_ok());
        assert!(create_provider("zhipu-cn", Some("key")).is_ok());
        assert!(create_provider("glm-global", Some("key")).is_ok());
        assert!(create_provider("bigmodel", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax() {
        assert!(create_provider("minimax", Some("key")).is_ok());
        assert!(create_provider("minimax-intl", Some("key")).is_ok());
        assert!(create_provider("minimax-io", Some("key")).is_ok());
        assert!(create_provider("minimax-cn", Some("key")).is_ok());
        assert!(create_provider("minimaxi", Some("key")).is_ok());
    }

    #[test]
    fn factory_bedrock() {
        assert!(create_provider("bedrock", Some("key")).is_ok());
        assert!(create_provider("aws-bedrock", Some("key")).is_ok());
    }

    #[test]
    fn factory_qianfan() {
        assert!(create_provider("qianfan", Some("key")).is_ok());
        assert!(create_provider("baidu", Some("key")).is_ok());
    }

    #[test]
    fn factory_qwen() {
        assert!(create_provider("qwen", Some("key")).is_ok());
        assert!(create_provider("dashscope", Some("key")).is_ok());
        assert!(create_provider("qwen-cn", Some("key")).is_ok());
        assert!(create_provider("dashscope-cn", Some("key")).is_ok());
        assert!(create_provider("qwen-intl", Some("key")).is_ok());
        assert!(create_provider("dashscope-intl", Some("key")).is_ok());
        assert!(create_provider("qwen-international", Some("key")).is_ok());
        assert!(create_provider("dashscope-international", Some("key")).is_ok());
        assert!(create_provider("qwen-us", Some("key")).is_ok());
        assert!(create_provider("dashscope-us", Some("key")).is_ok());
    }

    #[test]
    fn factory_lmstudio() {
        assert!(create_provider("lmstudio", Some("key")).is_ok());
        assert!(create_provider("lm-studio", Some("key")).is_ok());
        assert!(create_provider("lmstudio", None).is_ok());
    }

    // ── Extended ecosystem ───────────────────────────────────

    #[test]
    fn factory_groq() {
        assert!(create_provider("groq", Some("key")).is_ok());
    }

    #[test]
    fn factory_mistral() {
        assert!(create_provider("mistral", Some("key")).is_ok());
    }

    #[test]
    fn factory_xai() {
        assert!(create_provider("xai", Some("key")).is_ok());
        assert!(create_provider("grok", Some("key")).is_ok());
    }

    #[test]
    fn factory_deepseek() {
        assert!(create_provider("deepseek", Some("key")).is_ok());
    }

    #[test]
    fn factory_together() {
        assert!(create_provider("together", Some("key")).is_ok());
        assert!(create_provider("together-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_fireworks() {
        assert!(create_provider("fireworks", Some("key")).is_ok());
        assert!(create_provider("fireworks-ai", Some("key")).is_ok());
    }

    #[test]
    fn factory_perplexity() {
        assert!(create_provider("perplexity", Some("key")).is_ok());
    }

    #[test]
    fn factory_cohere() {
        assert!(create_provider("cohere", Some("key")).is_ok());
    }

    #[test]
    fn factory_copilot() {
        assert!(create_provider("copilot", Some("key")).is_ok());
        assert!(create_provider("github-copilot", Some("key")).is_ok());
    }

    #[test]
    fn factory_nvidia() {
        assert!(create_provider("nvidia", Some("nvapi-test")).is_ok());
        assert!(create_provider("nvidia-nim", Some("nvapi-test")).is_ok());
        assert!(create_provider("build.nvidia.com", Some("nvapi-test")).is_ok());
    }

    // ── AI inference routers ─────────────────────────────────

    #[test]
    fn factory_astrai() {
        assert!(create_provider("astrai", Some("sk-astrai-test")).is_ok());
    }

    // ── Custom / BYOP provider ─────────────────────────────

    #[test]
    fn factory_custom_url() {
        let p = create_provider("custom:https://my-llm.example.com", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_localhost() {
        let p = create_provider("custom:http://localhost:1234", Some("key"));
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_no_key() {
        let p = create_provider("custom:https://my-llm.example.com", None);
        assert!(p.is_ok());
    }

    #[test]
    fn factory_custom_empty_url_errors() {
        match create_provider("custom:", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a URL"),
                "Expected 'requires a URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for empty custom URL"),
        }
    }

    #[test]
    fn factory_custom_invalid_url_errors() {
        match create_provider("custom:not-a-url", None) {
            Err(e) => assert!(
                e.to_string().contains("requires a valid URL"),
                "Expected 'requires a valid URL', got: {e}"
            ),
            Ok(_) => panic!("Expected error for invalid custom URL"),
        }
    }

    #[test]
    fn factory_custom_unsupported_scheme_errors() {
        match create_provider("custom:ftp://example.com", None) {
            Err(e) => assert!(
                e.to_string().contains("http:// or https://"),
                "Expected scheme validation error, got: {e}"
            ),
            Ok(_) => panic!("Expected error for unsupported custom URL scheme"),
        }
    }

    #[test]
    fn factory_custom_trims_whitespace() {
        let p = create_provider("custom:  https://my-llm.example.com  ", Some("key"));
        assert!(p.is_ok());
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
    fn ollama_with_custom_url() {
        let provider = create_provider_with_url("ollama", None, Some("http://10.100.2.32:11434"));
        assert!(provider.is_ok());
    }

    #[test]
    fn ollama_cloud_with_custom_url() {
        let provider =
            create_provider_with_url("ollama", Some("ollama-key"), Some("https://ollama.com"));
        assert!(provider.is_ok());
    }

    #[test]
    fn factory_all_providers_create_successfully() {
        let providers = [
            "openrouter",
            "anthropic",
            "openai",
            "ollama",
            "gemini",
            "venice",
            "vercel",
            "cloudflare",
            "moonshot",
            "moonshot-intl",
            "moonshot-cn",
            "synthetic",
            "opencode",
            "zai",
            "zai-cn",
            "glm",
            "glm-cn",
            "minimax",
            "minimax-cn",
            "bedrock",
            "qianfan",
            "qwen",
            "qwen-intl",
            "qwen-cn",
            "qwen-us",
            "lmstudio",
            "groq",
            "mistral",
            "xai",
            "deepseek",
            "together",
            "fireworks",
            "perplexity",
            "cohere",
            "copilot",
            "nvidia",
        ];
        for name in providers {
            assert!(
                create_provider(name, Some("test-key")).is_ok(),
                "Provider '{name}' should create successfully"
            );
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
}
