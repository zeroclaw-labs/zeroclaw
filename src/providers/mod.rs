pub mod anthropic;
pub mod compatible;
pub mod gemini;
pub mod ollama;
pub mod openai;
pub mod openrouter;
pub mod traits;

pub use traits::Provider;

use compatible::{AuthStyle, OpenAiCompatibleProvider};

/// Factory: create the right provider from config
#[allow(clippy::too_many_lines)]
pub fn create_provider(name: &str, api_key: Option<&str>) -> anyhow::Result<Box<dyn Provider>> {
    match name {
        // ── Primary providers (custom implementations) ───────
        "openrouter" => Ok(Box::new(openrouter::OpenRouterProvider::new(api_key))),
        "anthropic" => Ok(Box::new(anthropic::AnthropicProvider::new(api_key))),
        "openai" => Ok(Box::new(openai::OpenAiProvider::new(api_key))),
        "ollama" => Ok(Box::new(ollama::OllamaProvider::new(
            api_key.filter(|k| !k.is_empty()),
        ))),
        "gemini" | "google" | "google-gemini" => {
            Ok(Box::new(gemini::GeminiProvider::new(api_key)))
        }

        // ── OpenAI-compatible providers ──────────────────────
        "venice" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Venice", "https://api.venice.ai", api_key, AuthStyle::Bearer,
        ))),
        "vercel" | "vercel-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Vercel AI Gateway", "https://api.vercel.ai", api_key, AuthStyle::Bearer,
        ))),
        "cloudflare" | "cloudflare-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Cloudflare AI Gateway",
            "https://gateway.ai.cloudflare.com/v1",
            api_key,
            AuthStyle::Bearer,
        ))),
        "moonshot" | "kimi" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Moonshot", "https://api.moonshot.cn", api_key, AuthStyle::Bearer,
        ))),
        "synthetic" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Synthetic", "https://api.synthetic.com", api_key, AuthStyle::Bearer,
        ))),
        "opencode" | "opencode-zen" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "OpenCode Zen", "https://api.opencode.ai", api_key, AuthStyle::Bearer,
        ))),
        "zai" | "z.ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Z.AI", "https://api.z.ai", api_key, AuthStyle::Bearer,
        ))),
        "glm" | "zhipu" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "GLM", "https://open.bigmodel.cn/api/paas", api_key, AuthStyle::Bearer,
        ))),
        "minimax" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "MiniMax", "https://api.minimax.chat", api_key, AuthStyle::Bearer,
        ))),
        "bedrock" | "aws-bedrock" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Amazon Bedrock",
            "https://bedrock-runtime.us-east-1.amazonaws.com",
            api_key,
            AuthStyle::Bearer,
        ))),
        "qianfan" | "baidu" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Qianfan", "https://aip.baidubce.com", api_key, AuthStyle::Bearer,
        ))),

        // ── Extended ecosystem (community favorites) ─────────
        "groq" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Groq", "https://api.groq.com/openai", api_key, AuthStyle::Bearer,
        ))),
        "mistral" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Mistral", "https://api.mistral.ai", api_key, AuthStyle::Bearer,
        ))),
        "xai" | "grok" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "xAI", "https://api.x.ai", api_key, AuthStyle::Bearer,
        ))),
        "deepseek" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "DeepSeek", "https://api.deepseek.com", api_key, AuthStyle::Bearer,
        ))),
        "together" | "together-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Together AI", "https://api.together.xyz", api_key, AuthStyle::Bearer,
        ))),
        "fireworks" | "fireworks-ai" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Fireworks AI", "https://api.fireworks.ai/inference", api_key, AuthStyle::Bearer,
        ))),
        "perplexity" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Perplexity", "https://api.perplexity.ai", api_key, AuthStyle::Bearer,
        ))),
        "cohere" => Ok(Box::new(OpenAiCompatibleProvider::new(
            "Cohere", "https://api.cohere.com/compatibility", api_key, AuthStyle::Bearer,
        ))),

        // ── Bring Your Own Provider (custom URL) ───────────
        // Format: "custom:https://your-api.com" or "custom:http://localhost:1234"
        name if name.starts_with("custom:") => {
            let base_url = name.strip_prefix("custom:").unwrap_or("");
            if base_url.is_empty() {
                anyhow::bail!("Custom provider requires a URL. Format: custom:https://your-api.com");
            }
            Ok(Box::new(OpenAiCompatibleProvider::new(
                "Custom",
                base_url,
                api_key,
                AuthStyle::Bearer,
            )))
        }

        _ => anyhow::bail!(
            "Unknown provider: {name}. Check README for supported providers or run `zeroclaw onboard --interactive` to reconfigure.\n\
             Tip: Use \"custom:https://your-api.com\" for any OpenAI-compatible endpoint."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Primary providers ────────────────────────────────────

    #[test]
    fn factory_openrouter() {
        assert!(create_provider("openrouter", Some("sk-test")).is_ok());
        assert!(create_provider("openrouter", None).is_ok());
    }

    #[test]
    fn factory_anthropic() {
        assert!(create_provider("anthropic", Some("sk-test")).is_ok());
    }

    #[test]
    fn factory_openai() {
        assert!(create_provider("openai", Some("sk-test")).is_ok());
    }

    #[test]
    fn factory_ollama() {
        assert!(create_provider("ollama", None).is_ok());
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
    }

    #[test]
    fn factory_glm() {
        assert!(create_provider("glm", Some("key")).is_ok());
        assert!(create_provider("zhipu", Some("key")).is_ok());
    }

    #[test]
    fn factory_minimax() {
        assert!(create_provider("minimax", Some("key")).is_ok());
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
            "synthetic",
            "opencode",
            "zai",
            "glm",
            "minimax",
            "bedrock",
            "qianfan",
            "groq",
            "mistral",
            "xai",
            "deepseek",
            "together",
            "fireworks",
            "perplexity",
            "cohere",
        ];
        for name in providers {
            assert!(
                create_provider(name, Some("test-key")).is_ok(),
                "Provider '{name}' should create successfully"
            );
        }
    }
}
