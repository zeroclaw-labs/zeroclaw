//! Free-tier provider catalog.
//!
//! Known LLM providers that expose a free tier or no-key trial endpoint,
//! with their console URLs and a sensible default model. This catalog
//! exists so the onboarding UX and `RouterModelProvider` configuration
//! can offer a curated "free mode" without forcing users to discover
//! each upstream's quirks.
//!
//! Pattern adapted from claurst's `free.rs` `FREE_CATALOG`. Order is
//! priority — fastest/most generous tiers first — so a router seeded
//! from this list iterates in the same order.

/// One upstream provider in the free-mode catalog.
///
/// `id` is the canonical provider-family key used in `[model_providers.<id>]`
/// config sections, in `RouterModelProvider` route names, and as the prefix
/// users type for `<id>/<model>` pinning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FreeUpstream {
    pub id: &'static str,
    pub title: &'static str,
    pub key_url: &'static str,
    pub default_model: &'static str,
    pub note: &'static str,
}

/// Ordered priority of providers we recommend stacking into a free-mode
/// router. Put the fastest / most generous tiers first.
pub const FREE_CATALOG: &[FreeUpstream] = &[
    FreeUpstream {
        id: "groq",
        title: "Groq",
        key_url: "console.groq.com/keys",
        default_model: "llama-3.3-70b-versatile",
        note: "fast — Llama 3.3, GPT-OSS, Qwen3",
    },
    FreeUpstream {
        id: "cerebras",
        title: "Cerebras",
        key_url: "cloud.cerebras.ai",
        default_model: "qwen-3-235b-a22b-instruct-2507",
        note: "wafer-scale — Qwen3 235B",
    },
    FreeUpstream {
        id: "google",
        title: "Google Gemini",
        key_url: "aistudio.google.com/app/apikey",
        default_model: "gemini-2.5-flash",
        note: "Gemini 2.5 Flash free tier",
    },
    FreeUpstream {
        id: "mistral",
        title: "Mistral",
        key_url: "console.mistral.ai/api-keys",
        default_model: "mistral-large-latest",
        note: "Large · Medium · Codestral · Devstral",
    },
    FreeUpstream {
        id: "sambanova",
        title: "SambaNova",
        key_url: "cloud.sambanova.ai",
        default_model: "Meta-Llama-3.3-70B-Instruct",
        note: "DeepSeek V3 · Llama 4 · Gemma 3",
    },
    FreeUpstream {
        id: "nvidia",
        title: "NVIDIA NIM",
        key_url: "build.nvidia.com",
        default_model: "meta/llama-3.3-70b-instruct",
        note: "NIM endpoints (trial credits)",
    },
    FreeUpstream {
        id: "cohere",
        title: "Cohere",
        key_url: "dashboard.cohere.com/api-keys",
        default_model: "command-r-plus",
        note: "Command R+ trial tier",
    },
    FreeUpstream {
        id: "openrouter",
        title: "OpenRouter",
        key_url: "openrouter.ai/keys",
        default_model: "openrouter/auto",
        note: "free-tier models — $10 top-up lifts caps",
    },
    FreeUpstream {
        id: "zai",
        title: "Z.AI",
        key_url: "z.ai/manage-apikey/apikey-list",
        default_model: "glm-4.6",
        note: "GLM-4.6 / GLM-4.7",
    },
    FreeUpstream {
        id: "zhipuai",
        title: "Zhipu",
        key_url: "open.bigmodel.cn",
        default_model: "glm-4.5",
        note: "GLM-4.5 (CN endpoint)",
    },
    FreeUpstream {
        id: "ollama",
        title: "Ollama (local)",
        key_url: "ollama.com/download",
        default_model: "llama3.2",
        note: "fully local — no key, no internet",
    },
];

/// Look up a catalog entry by its `id`. Returns `None` for unknown ids.
pub fn lookup(id: &str) -> Option<&'static FreeUpstream> {
    FREE_CATALOG.iter().find(|e| e.id == id)
}

/// Ordered list of catalog ids, useful for seeding a `RouterModelProvider`
/// in the same priority claurst's free-mode router uses.
pub fn priority_order() -> impl Iterator<Item = &'static str> {
    FREE_CATALOG.iter().map(|e| e.id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_is_non_empty_and_ids_are_unique() {
        assert!(!FREE_CATALOG.is_empty());
        let mut seen = std::collections::HashSet::new();
        for entry in FREE_CATALOG {
            assert!(
                seen.insert(entry.id),
                "duplicate id in FREE_CATALOG: {}",
                entry.id
            );
        }
    }

    #[test]
    fn lookup_returns_entry_for_known_id() {
        let entry = lookup("groq").expect("groq must be in catalog");
        assert_eq!(entry.title, "Groq");
    }

    #[test]
    fn lookup_returns_none_for_unknown_id() {
        assert!(lookup("definitely-not-a-real-provider").is_none());
    }

    #[test]
    fn priority_order_yields_all_ids() {
        let collected: Vec<&str> = priority_order().collect();
        assert_eq!(collected.len(), FREE_CATALOG.len());
        // First entry should be Groq (fastest free tier).
        assert_eq!(collected[0], "groq");
    }
}
