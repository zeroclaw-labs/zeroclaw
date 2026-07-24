//! Per-family catalog source table.

use std::time::Duration;

use anyhow::Result;
use serde::Deserialize;

const NEARAI_CATALOG_URL: &str = "https://cloud-api.near.ai/v1/model/list";
const FETCH_TIMEOUT_SECS: u64 = 10;

/// `(models.dev key, openrouter.ai vendor prefix)` for a family name.
/// Either or both can be `None` for families with no public catalog
/// (local-only servers, credential-required APIs without a public
/// `/models` index).
#[must_use]
pub fn catalog_source_for(family: &str) -> Option<(Option<&'static str>, Option<&'static str>)> {
    let pair: (Option<&'static str>, Option<&'static str>) = match family {
        // First-party / bespoke factories.
        "openai" => (Some("openai"), Some("openai")),
        "anthropic" => (Some("anthropic"), Some("anthropic")),
        "azure" => (Some("azure"), None),
        "bedrock" => (Some("amazon-bedrock"), None),
        "gemini" => (Some("google"), Some("google")),
        "gemini_cli" => (Some("google"), Some("google")),
        "openrouter" => (Some("openrouter"), Some("openrouter")),
        "copilot" => (Some("github-copilot"), None),
        "minimax" => (Some("minimax"), Some("minimax")),
        "lmstudio" => (Some("lmstudio"), None),
        "kilocli" => (Some("kilo"), None),
        "kilo" => (Some("kilo"), None),
        "ovh" => (Some("ovhcloud"), None),
        // Compat families — mirrors the consts in CompatFamilySpec impls.
        "moonshot" => (Some("moonshotai"), Some("moonshotai")),
        "qwen" => (Some("alibaba"), Some("qwen")),
        "glm" => (Some("zhipuai"), None),
        "zai" => (Some("zai"), Some("z-ai")),
        "doubao" => (None, Some("bytedance")),
        "hunyuan" => (None, Some("tencent")),
        "qianfan" => (None, Some("baidu")),
        "groq" => (Some("groq"), None),
        "mistral" => (Some("mistral"), Some("mistralai")),
        "deepseek" => (Some("deepseek"), Some("deepseek")),
        "together" => (Some("togetherai"), None),
        "fireworks" => (Some("fireworks-ai"), None),
        "cohere" => (Some("cohere"), Some("cohere")),
        "perplexity" => (Some("perplexity"), Some("perplexity")),
        "xai" => (Some("xai"), Some("x-ai")),
        "cerebras" => (Some("cerebras"), None),
        "deepinfra" => (Some("deepinfra"), None),
        "huggingface" => (Some("huggingface"), None),
        "ai21" => (None, Some("ai21")),
        "reka" => (None, Some("rekaai")),
        "baseten" => (Some("baseten"), None),
        "nebius" => (Some("nebius"), None),
        "friendli" => (Some("friendli"), None),
        "stepfun" => (Some("stepfun"), Some("stepfun")),
        "aihubmix" => (Some("aihubmix"), None),
        "siliconflow" => (Some("siliconflow"), None),
        "venice" => (Some("venice"), None),
        // NEAR AI Cloud publishes its own no-auth catalog at /v1/model/list.
        // `list_models_for_family` handles that path before using this tuple.
        "nearai" => (None, None),
        "novita" => (Some("novita-ai"), None),
        "nvidia" => (Some("nvidia"), Some("nvidia")),
        "vercel" => (Some("vercel"), None),
        "cloudflare" => (Some("cloudflare-ai-gateway"), None),
        "synthetic" => (Some("synthetic"), None),
        "opencode" => (Some("opencode"), None),
        "atomic_chat" => (Some("atomic-chat"), None),
        "telnyx" => (None, None),
        // Families with no public catalog: local-only servers (no public
        // /models index without a running server) or credential-required
        // APIs with no published catalog. Operator pastes a credential and
        // the provider's `/models` endpoint serves the list directly.
        "sambanova" | "hyperbolic" | "anyscale" | "nscale" | "lepton" | "yi" | "baichuan"
        | "avian" | "deepmyst" | "astrai" | "sglang" | "vllm" | "osaurus" | "litellm"
        | "llamacpp" | "ollama" | "hailo_ollama" | "manifest" | "morph" | "github_models"
        | "upstage" | "featherless" | "arcee" | "lambda_ai" | "inception" | "custom" => {
            (None, None)
        }
        _ => return None,
    };
    Some(pair)
}

#[derive(Debug, Deserialize)]
struct NearaiCatalog {
    #[serde(default)]
    models: Vec<NearaiModel>,
}

#[derive(Debug, Deserialize)]
struct NearaiModel {
    #[serde(rename = "modelId")]
    model_id: String,
    #[serde(default)]
    metadata: NearaiModelMetadata,
}

#[derive(Debug, Default, Deserialize)]
struct NearaiModelMetadata {
    #[serde(default)]
    architecture: NearaiArchitecture,
}

#[derive(Debug, Default, Deserialize)]
struct NearaiArchitecture {
    #[serde(default, rename = "outputModalities")]
    output_modalities: Vec<String>,
}

fn is_nearai_chat_model(model: &NearaiModel) -> bool {
    // Brittle: relies on NEAR keeping the "privacy-filter" substring in
    // audit-class model IDs. Revisit if NEAR exposes an architectural
    // classifier (e.g. a `model_class` or `endpoint_type` field) in the
    // catalog metadata.
    if model.model_id.contains("privacy-filter") {
        return false;
    }

    let arch = &model.metadata.architecture;
    arch.output_modalities
        .iter()
        .any(|modality| modality == "text")
}

pub(crate) fn parse_nearai_catalog(bytes: &[u8]) -> Result<Vec<String>> {
    let catalog: NearaiCatalog = serde_json::from_slice(bytes)?;
    let mut ids: Vec<String> = catalog
        .models
        .iter()
        .filter(|model| is_nearai_chat_model(model))
        .map(|model| model.model_id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn list_nearai_models() -> Result<Vec<String>> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .build()?;
    let response = client
        .get(NEARAI_CATALOG_URL)
        .send()
        .await?
        .error_for_status()?;
    let bytes = response.bytes().await?;
    parse_nearai_catalog(&bytes)
}

/// Probe the catalog for `family` without constructing a live provider.
/// Returns the union of every known public catalog source. Errors if
/// `family` is unknown or has no public catalog source set.
pub async fn list_models_for_family(family: &str) -> Result<Vec<String>> {
    if family == "nearai" {
        return list_nearai_models().await;
    }

    let Some((md_key, or_prefix)) = catalog_source_for(family) else {
        anyhow::bail!("unknown provider family {family:?}");
    };
    if let Some(k) = md_key
        && let Ok(ms) = crate::models_dev::list_models_for(k).await
        && !ms.is_empty()
    {
        return Ok(ms);
    }
    if let Some(p) = or_prefix {
        return crate::openrouter_catalog::list_models_for_vendor(p).await;
    }
    anyhow::bail!("no public catalog for family {family:?}")
}

#[must_use]
pub fn sort_model_catalog_for_chat(provider: &str, models: Vec<String>) -> Option<Vec<String>> {
    let provider_l = provider.to_ascii_lowercase();
    let mut ranked: Vec<(i32, String, String)> = models
        .into_iter()
        .filter_map(|model| {
            let model_l = model.to_ascii_lowercase();
            if is_non_chat_model_lower(&model_l) {
                None
            } else {
                Some((chat_model_rank_lower(&provider_l, &model_l), model_l, model))
            }
        })
        .collect();
    if ranked.is_empty() {
        return None;
    }
    ranked.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    Some(ranked.into_iter().map(|(_, _, model)| model).collect())
}

fn chat_model_rank_lower(provider: &str, model_l: &str) -> i32 {
    let mut rank = 100;

    if provider.starts_with("openai") {
        if model_l.starts_with("gpt-5") {
            rank -= 70;
        } else if model_l.starts_with("gpt-4.1")
            || model_l.starts_with("gpt-4o")
            || model_l.starts_with("o3")
            || model_l.starts_with("o4")
        {
            rank -= 55;
        } else if model_l.starts_with("gpt-3.5") {
            rank -= 10;
        }
    } else if contains_any(
        model_l,
        &[
            "claude", "sonnet", "opus", "gpt", "gemini", "deepseek", "qwen", "kimi", "llama",
            "mistral", "grok", "coder", "code", "reason", "r1", "o3", "o4",
        ],
    ) {
        rank -= 45;
    }

    rank
}

fn is_non_chat_model_lower(model: &str) -> bool {
    contains_any(
        model,
        &[
            "image",
            "audio",
            "tts",
            "transcribe",
            "embedding",
            "moderation",
            "realtime",
            "whisper",
        ],
    )
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_canonical_family_has_a_catalog_table_entry() {
        macro_rules! collect_family_names {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                vec![$($type_str),+]
            };
        }
        let families: Vec<&str> =
            zeroclaw_config::for_each_model_provider_slot!(collect_family_names);
        let mut missing: Vec<&str> = Vec::new();
        for family in &families {
            if catalog_source_for(family).is_none() {
                missing.push(family);
            }
        }
        assert!(
            missing.is_empty(),
            "catalog_source_for is missing entries for: {missing:?}"
        );
    }

    #[test]
    fn unknown_family_returns_none() {
        assert!(catalog_source_for("not_a_real_provider").is_none());
    }

    #[test]
    fn known_family_with_dual_sources_returns_both() {
        let (md, or) = catalog_source_for("xai").expect("xai is canonical");
        assert_eq!(md, Some("xai"));
        assert_eq!(or, Some("x-ai"));
    }

    #[test]
    fn local_only_family_returns_no_sources() {
        let (md, or) = catalog_source_for("llamacpp").expect("llamacpp is canonical");
        assert_eq!(md, None);
        assert_eq!(or, None);
    }

    #[test]
    fn nearai_family_uses_provider_catalog_source() {
        let (md, or) = catalog_source_for("nearai").expect("nearai is canonical");
        assert_eq!(md, None);
        assert_eq!(or, None);
    }

    #[test]
    fn parse_nearai_catalog_filters_to_chat_model_ids() {
        let raw = r#"{
            "models": [
                {
                    "modelId": "zai-org/GLM-5.1-FP8",
                    "metadata": {
                        "architecture": {
                            "inputModalities": ["text"],
                            "outputModalities": ["text"]
                        }
                    }
                },
                {
                    "modelId": "openai/privacy-filter",
                    "metadata": {
                        "architecture": {
                            "inputModalities": ["text"],
                            "outputModalities": ["text"]
                        }
                    }
                },
                {
                    "modelId": "Qwen/Qwen3-Embedding-0.6B",
                    "metadata": {
                        "architecture": {
                            "inputModalities": ["text"],
                            "outputModalities": ["embedding"]
                        }
                    }
                },
                {
                    "modelId": "incomplete",
                    "metadata": {
                        "architecture": {
                            "inputModalities": [],
                            "outputModalities": []
                        }
                    }
                }
            ]
        }"#;
        let ids = parse_nearai_catalog(raw.as_bytes()).unwrap();
        assert_eq!(ids, vec!["zai-org/GLM-5.1-FP8"]);
    }

    #[test]
    fn bespoke_family_with_only_models_dev() {
        let (md, or) = catalog_source_for("azure").expect("azure is canonical");
        assert_eq!(md, Some("azure"));
        assert_eq!(or, None);
    }

    #[test]
    fn bespoke_family_with_only_openrouter() {
        let (md, or) = catalog_source_for("ai21").expect("ai21 is canonical");
        assert_eq!(md, None);
        assert_eq!(or, Some("ai21"));
    }

    #[test]
    fn quickstart_sort_prefers_chat_and_coding_models() {
        let sorted = sort_model_catalog_for_chat(
            "openai",
            vec![
                "chatgpt-image-latest".into(),
                "text-embedding-ada-002".into(),
                "gpt-3.5-turbo".into(),
                "gpt-5".into(),
                "tts-1".into(),
            ],
        )
        .expect("chat model catalog");

        assert_eq!(sorted[0], "gpt-5");
        assert!(!sorted.iter().any(|m| m == "chatgpt-image-latest"));
        assert!(!sorted.iter().any(|m| m == "text-embedding-ada-002"));
        assert!(!sorted.iter().any(|m| m == "tts-1"));

        let sorted = sort_model_catalog_for_chat(
            "openrouter",
            vec![
                "ai21/jamba-mini".into(),
                "openai/gpt-4.1".into(),
                "some/image-model".into(),
                "anthropic/claude-sonnet-4".into(),
            ],
        )
        .expect("chat model catalog");
        assert_eq!(sorted[0], "anthropic/claude-sonnet-4");
        assert_eq!(sorted[1], "openai/gpt-4.1");
        assert!(!sorted.iter().any(|m| m == "some/image-model"));
    }

    #[test]
    fn quickstart_sort_returns_none_when_only_non_chat_models_exist() {
        let sorted = sort_model_catalog_for_chat(
            "openai",
            vec!["gpt-image-1.5".into(), "text-embedding-ada-002".into()],
        );

        assert!(sorted.is_none());
    }
}
