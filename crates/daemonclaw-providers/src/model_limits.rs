//! Static lookup table for model context window sizes.
//!
//! When the config says `max_context_tokens = 0` (unlimited), the system
//! resolves the actual limit through: provider query → this table → super
//! fallback.  Entries are (prefix, context_window_tokens); longest prefix
//! match wins so more-specific entries override family defaults.

const SUPER_FALLBACK_CONTEXT_WINDOW: usize = 128_000;

struct ModelEntry {
    prefix: &'static str,
    context_window: usize,
}

// Sorted longest-prefix-first within each provider group.
// Sources: official provider documentation as of 2026-05.
static MODEL_TABLE: &[ModelEntry] = &[
    // ── OpenAI ──────────────────────────────────────────────────────
    ModelEntry { prefix: "o3-pro",                       context_window: 200_000 },
    ModelEntry { prefix: "o3-mini",                      context_window: 200_000 },
    ModelEntry { prefix: "o3",                           context_window: 200_000 },
    ModelEntry { prefix: "o4-mini",                      context_window: 200_000 },
    ModelEntry { prefix: "o1-pro",                       context_window: 200_000 },
    ModelEntry { prefix: "o1-mini",                      context_window: 128_000 },
    ModelEntry { prefix: "o1-preview",                   context_window: 128_000 },
    ModelEntry { prefix: "o1",                           context_window: 200_000 },
    ModelEntry { prefix: "gpt-4.1-mini",                 context_window: 1_047_576 },
    ModelEntry { prefix: "gpt-4.1-nano",                 context_window: 1_047_576 },
    ModelEntry { prefix: "gpt-4.1",                      context_window: 1_047_576 },
    ModelEntry { prefix: "gpt-4.5-preview",              context_window: 128_000 },
    ModelEntry { prefix: "gpt-4o-mini",                  context_window: 128_000 },
    ModelEntry { prefix: "gpt-4o",                       context_window: 128_000 },
    ModelEntry { prefix: "gpt-4-turbo",                  context_window: 128_000 },
    ModelEntry { prefix: "gpt-4-1106",                   context_window: 128_000 },
    ModelEntry { prefix: "gpt-4-0125",                   context_window: 128_000 },
    ModelEntry { prefix: "gpt-4",                        context_window: 8_192 },
    ModelEntry { prefix: "gpt-3.5-turbo-0125",           context_window: 16_385 },
    ModelEntry { prefix: "gpt-3.5-turbo-1106",           context_window: 16_385 },
    ModelEntry { prefix: "gpt-3.5-turbo-16k",            context_window: 16_385 },
    ModelEntry { prefix: "gpt-3.5-turbo",                context_window: 16_385 },
    // ── Anthropic ───────────────────────────────────────────────────
    ModelEntry { prefix: "claude-opus-4",                context_window: 200_000 },
    ModelEntry { prefix: "claude-sonnet-4",              context_window: 200_000 },
    ModelEntry { prefix: "claude-3.7-sonnet",            context_window: 200_000 },
    ModelEntry { prefix: "claude-3.5-sonnet",            context_window: 200_000 },
    ModelEntry { prefix: "claude-3.5-haiku",             context_window: 200_000 },
    ModelEntry { prefix: "claude-3-opus",                context_window: 200_000 },
    ModelEntry { prefix: "claude-3-sonnet",              context_window: 200_000 },
    ModelEntry { prefix: "claude-3-haiku",               context_window: 200_000 },
    ModelEntry { prefix: "claude-2",                     context_window: 100_000 },
    ModelEntry { prefix: "claude-instant",               context_window: 100_000 },
    ModelEntry { prefix: "claude",                       context_window: 200_000 },
    // ── Google Gemini ───────────────────────────────────────────────
    ModelEntry { prefix: "gemini-2.5-pro",               context_window: 1_048_576 },
    ModelEntry { prefix: "gemini-2.5-flash",             context_window: 1_048_576 },
    ModelEntry { prefix: "gemini-2.0-flash-lite",        context_window: 1_048_576 },
    ModelEntry { prefix: "gemini-2.0-flash",             context_window: 1_048_576 },
    ModelEntry { prefix: "gemini-1.5-pro",               context_window: 2_097_152 },
    ModelEntry { prefix: "gemini-1.5-flash",             context_window: 1_048_576 },
    ModelEntry { prefix: "gemini-1.0-pro",               context_window: 32_760 },
    ModelEntry { prefix: "gemini",                       context_window: 1_048_576 },
    // ── GLM / Z.AI ──────────────────────────────────────────────────
    ModelEntry { prefix: "glm-5-turbo",                  context_window: 128_000 },
    ModelEntry { prefix: "glm-5-plus",                   context_window: 128_000 },
    ModelEntry { prefix: "glm-5-air",                    context_window: 128_000 },
    ModelEntry { prefix: "glm-5",                        context_window: 128_000 },
    ModelEntry { prefix: "glm-4-long",                   context_window: 1_000_000 },
    ModelEntry { prefix: "glm-4-plus",                   context_window: 128_000 },
    ModelEntry { prefix: "glm-4-air",                    context_window: 128_000 },
    ModelEntry { prefix: "glm-4-flash",                  context_window: 128_000 },
    ModelEntry { prefix: "glm-4-alltools",               context_window: 128_000 },
    ModelEntry { prefix: "glm-4",                        context_window: 128_000 },
    ModelEntry { prefix: "glm-3-turbo",                  context_window: 128_000 },
    // ── DeepSeek ────────────────────────────────────────────────────
    ModelEntry { prefix: "deepseek-r1",                  context_window: 128_000 },
    ModelEntry { prefix: "deepseek-v3",                  context_window: 128_000 },
    ModelEntry { prefix: "deepseek-chat",                context_window: 128_000 },
    ModelEntry { prefix: "deepseek-coder",               context_window: 128_000 },
    ModelEntry { prefix: "deepseek-reasoner",            context_window: 128_000 },
    ModelEntry { prefix: "deepseek",                     context_window: 128_000 },
    // ── Qwen ────────────────────────────────────────────────────────
    ModelEntry { prefix: "qwen3-235b",                   context_window: 131_072 },
    ModelEntry { prefix: "qwen3-32b",                    context_window: 131_072 },
    ModelEntry { prefix: "qwen3-30b",                    context_window: 131_072 },
    ModelEntry { prefix: "qwen3-14b",                    context_window: 131_072 },
    ModelEntry { prefix: "qwen3-8b",                     context_window: 131_072 },
    ModelEntry { prefix: "qwen3-4b",                     context_window: 131_072 },
    ModelEntry { prefix: "qwen3-1.7b",                   context_window: 32_768 },
    ModelEntry { prefix: "qwen3-0.6b",                   context_window: 32_768 },
    ModelEntry { prefix: "qwen3",                        context_window: 131_072 },
    ModelEntry { prefix: "qwen2.5-coder",                context_window: 131_072 },
    ModelEntry { prefix: "qwen2.5",                      context_window: 131_072 },
    ModelEntry { prefix: "qwen2",                        context_window: 131_072 },
    ModelEntry { prefix: "qwen-turbo",                   context_window: 1_000_000 },
    ModelEntry { prefix: "qwen-plus",                    context_window: 131_072 },
    ModelEntry { prefix: "qwen-max",                     context_window: 32_768 },
    ModelEntry { prefix: "qwen-long",                    context_window: 10_000_000 },
    ModelEntry { prefix: "qwen",                         context_window: 131_072 },
    // ── Meta Llama ──────────────────────────────────────────────────
    ModelEntry { prefix: "llama-4",                      context_window: 131_072 },
    ModelEntry { prefix: "llama-3.3",                    context_window: 131_072 },
    ModelEntry { prefix: "llama-3.2",                    context_window: 131_072 },
    ModelEntry { prefix: "llama-3.1",                    context_window: 131_072 },
    ModelEntry { prefix: "llama-3",                      context_window: 8_192 },
    ModelEntry { prefix: "llama3",                       context_window: 8_192 },
    ModelEntry { prefix: "llama",                        context_window: 131_072 },
    // ── Mistral ─────────────────────────────────────────────────────
    ModelEntry { prefix: "mistral-large",                context_window: 131_072 },
    ModelEntry { prefix: "mistral-medium",               context_window: 131_072 },
    ModelEntry { prefix: "mistral-small",                context_window: 131_072 },
    ModelEntry { prefix: "mistral-nemo",                 context_window: 131_072 },
    ModelEntry { prefix: "codestral",                    context_window: 256_000 },
    ModelEntry { prefix: "pixtral-large",                context_window: 131_072 },
    ModelEntry { prefix: "pixtral",                      context_window: 131_072 },
    ModelEntry { prefix: "ministral",                    context_window: 131_072 },
    ModelEntry { prefix: "mistral",                      context_window: 131_072 },
    // ── Cohere ──────────────────────────────────────────────────────
    ModelEntry { prefix: "command-a",                    context_window: 256_000 },
    ModelEntry { prefix: "command-r-plus",               context_window: 128_000 },
    ModelEntry { prefix: "command-r",                    context_window: 128_000 },
    ModelEntry { prefix: "command",                      context_window: 128_000 },
    // ── Moonshot / Kimi ─────────────────────────────────────────────
    ModelEntry { prefix: "moonshot-v1-128k",             context_window: 128_000 },
    ModelEntry { prefix: "moonshot-v1-32k",              context_window: 32_000 },
    ModelEntry { prefix: "moonshot-v1-8k",               context_window: 8_000 },
    ModelEntry { prefix: "moonshot",                     context_window: 128_000 },
    // ── MiniMax ─────────────────────────────────────────────────────
    ModelEntry { prefix: "MiniMax-M3",                   context_window: 1_000_000 },
    ModelEntry { prefix: "MiniMax-Text-01",              context_window: 1_000_000 },
    ModelEntry { prefix: "abab7",                        context_window: 245_760 },
    ModelEntry { prefix: "abab6.5",                      context_window: 245_760 },
    ModelEntry { prefix: "abab",                         context_window: 245_760 },
    // ── xAI Grok ────────────────────────────────────────────────────
    ModelEntry { prefix: "grok-3",                       context_window: 131_072 },
    ModelEntry { prefix: "grok-2",                       context_window: 131_072 },
    ModelEntry { prefix: "grok",                         context_window: 131_072 },
    // ── Yi ──────────────────────────────────────────────────────────
    ModelEntry { prefix: "yi-lightning",                  context_window: 16_384 },
    ModelEntry { prefix: "yi-large-turbo",               context_window: 16_384 },
    ModelEntry { prefix: "yi-large",                     context_window: 32_768 },
    ModelEntry { prefix: "yi",                           context_window: 16_384 },
    // ── Microsoft Phi ───────────────────────────────────────────────
    ModelEntry { prefix: "phi-4",                        context_window: 16_384 },
    ModelEntry { prefix: "phi-3.5",                      context_window: 131_072 },
    ModelEntry { prefix: "phi-3",                        context_window: 131_072 },
    ModelEntry { prefix: "phi",                          context_window: 16_384 },
];

/// Look up a model's context window from the static table.
/// Returns 0 if the model isn't recognized.
pub fn lookup_model_context_window(model: &str) -> usize {
    let lower = model.to_lowercase();
    // OpenRouter-style "provider/model" — strip the provider prefix.
    let normalized = lower
        .rsplit_once('/')
        .map_or(lower.as_str(), |(_, m)| m);

    for entry in MODEL_TABLE {
        if normalized.starts_with(entry.prefix) {
            return entry.context_window;
        }
    }
    0
}

/// Resolve the effective context window for a model.
///
/// Resolution order:
/// 1. Provider runtime query (`context_window_for_model`)
/// 2. Static model table lookup
/// 3. Super fallback (128K)
pub fn resolve_context_window(provider_reported: usize, model: &str) -> usize {
    if provider_reported > 0 {
        return provider_reported;
    }
    let table_value = lookup_model_context_window(model);
    if table_value > 0 {
        return table_value;
    }
    SUPER_FALLBACK_CONTEXT_WINDOW
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_model_lookup() {
        assert_eq!(lookup_model_context_window("glm-5-turbo"), 128_000);
        assert_eq!(lookup_model_context_window("gpt-4o"), 128_000);
        assert_eq!(lookup_model_context_window("claude-3-opus"), 200_000);
        assert_eq!(lookup_model_context_window("gemini-2.5-pro"), 1_048_576);
    }

    #[test]
    fn prefix_match_with_version_suffix() {
        assert_eq!(lookup_model_context_window("gpt-4o-2024-08-06"), 128_000);
        assert_eq!(lookup_model_context_window("claude-3.5-sonnet-20240620"), 200_000);
        assert_eq!(lookup_model_context_window("glm-5-turbo-latest"), 128_000);
    }

    #[test]
    fn openrouter_style_prefix_stripped() {
        assert_eq!(lookup_model_context_window("openai/gpt-4o"), 128_000);
        assert_eq!(lookup_model_context_window("anthropic/claude-3-opus"), 200_000);
        assert_eq!(lookup_model_context_window("google/gemini-2.5-flash"), 1_048_576);
    }

    #[test]
    fn unknown_model_returns_zero() {
        assert_eq!(lookup_model_context_window("totally-unknown-model"), 0);
    }

    #[test]
    fn resolve_prefers_provider_then_table_then_fallback() {
        assert_eq!(resolve_context_window(200_000, "glm-5-turbo"), 200_000);
        assert_eq!(resolve_context_window(0, "glm-5-turbo"), 128_000);
        assert_eq!(resolve_context_window(0, "totally-unknown-model"), SUPER_FALLBACK_CONTEXT_WINDOW);
    }

    #[test]
    fn case_insensitive() {
        assert_eq!(lookup_model_context_window("GPT-4o"), 128_000);
        assert_eq!(lookup_model_context_window("Claude-3-Opus"), 200_000);
    }

    #[test]
    fn longer_prefix_wins_over_shorter() {
        assert_eq!(lookup_model_context_window("gpt-4o-mini"), 128_000);
        assert_eq!(lookup_model_context_window("gpt-4-turbo-preview"), 128_000);
        assert_eq!(lookup_model_context_window("gpt-4-0125-preview"), 128_000);
        // gpt-4 (no suffix) → 8K, not 128K
        assert_eq!(lookup_model_context_window("gpt-4"), 8_192);
    }
}
