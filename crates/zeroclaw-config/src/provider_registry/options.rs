//! One field definition for API defaults, named presets and model patches.

use serde::{Deserialize, Serialize};

use super::Override;
use crate::schema::{CacheTtl, ToolResultImagePolicy, WireApi};

pub(crate) fn option_fields(capability: &str) -> &'static [&'static str] {
    match capability {
        "chat" => ChatOptionsPatch::FIELDS,
        "tts" => TtsOptionsPatch::FIELDS,
        "asr" => AsrOptionsPatch::FIELDS,
        _ => &[],
    }
}

macro_rules! options_patch {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(default)]
        pub struct $name {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            pub $field: Option<$type>,)*
        }

        impl $name {
            /// Field names and patch decoding are generated from the same
            /// declaration. Only the load report uses this for diagnostics.
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];

            pub fn overlay(&mut self, higher: &Self) {
                $(if higher.$field.is_some() {
                    self.$field.clone_from(&higher.$field);
                })*
            }

            pub fn is_empty(&self) -> bool {
                true $(&& self.$field.is_none())*
            }
        }
    };
}

options_patch!(ChatOptionsPatch {
    temperature: Override<f64>,
    max_tokens: Override<u32>,
    think: Override<bool>,
    cache_ttl: Override<CacheTtl>,
    native_tools: bool,
    chat_template_kwargs: serde_json::Value,
    provider_extra: serde_json::Value,
    merge_system_into_user: bool,
    replay_assistant_reasoning: bool,
    tool_result_image_policy: ToolResultImagePolicy,
    thinking_passthrough: bool,
    reasoning_effort_passthrough: bool,
    cache_passthrough: bool,
    extract_text_reasoning: bool,
    extract_text_tool_calls: bool,
});

/// Effective model-owned textual extraction policy. Missing options inherit;
/// explicit values are ordinary booleans and never accept the `"null"` sentinel.
pub use zeroclaw_api::TextExtraction;

/// Provider-specific baselines for the two model options. This is the
/// sole declaration of these defaults, shared by canonical and legacy
/// selections; it does not describe the upstream's native capabilities.
pub(crate) fn text_extraction_defaults(
    kind: &str,
    wire_api: Option<WireApi>,
    native_tools: Option<bool>,
) -> TextExtraction {
    // MiniMax's unsplit reasoning and Groq's raw reasoning format put
    // <think> blocks in content. Other providers must
    // opt in when a particular model/server emits inline reasoning.
    let reasoning = matches!(kind, "minimax" | "groq");
    // These adapters currently use prompt-guided tool calls by default.
    // Several upstreams also support native tools; enabling that mode
    // removes the need to infer calls from arbitrary response text.
    let native_tool_mode = match kind {
        "groq" => native_tools == Some(true),
        "venice" | "atomic_chat" => {
            native_tools == Some(true) || wire_api == Some(WireApi::Responses)
        }
        _ => false,
    };
    let tool_calls = !native_tool_mode
        && matches!(
            kind,
            "ollama"
                | "hailo_ollama"
                | "gemini"
                | "gemini_cli"
                | "grok_cli"
                | "kilocli"
                | "groq"
                | "venice"
                | "atomic_chat"
        );
    TextExtraction {
        reasoning,
        tool_calls,
    }
}

options_patch!(TtsOptionsPatch {
    voice: String,
    language: String,
    speed: Override<f64>,
    stability: Override<f64>,
    similarity_boost: Override<f64>,
    format: Override<String>,
});

options_patch!(AsrOptionsPatch {
    language: Override<String>,
    initial_prompt: Override<String>,
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct Preset<O: Default> {
    #[serde(default)]
    pub options: O,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "capability", rename_all = "lowercase")]
pub enum ModelPreset {
    Chat(Preset<ChatOptionsPatch>),
    Tts(Preset<TtsOptionsPatch>),
    Asr(Preset<AsrOptionsPatch>),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_keeps_clear_false_and_zero_until_final_resolution() {
        let mut low: ChatOptionsPatch =
            toml::from_str("temperature = 0.7\nthink = true\nmax_tokens = 32").unwrap();
        let high: ChatOptionsPatch =
            toml::from_str("temperature = 0.0\nthink = false\nmax_tokens = \"null\"").unwrap();
        low.overlay(&high);
        assert_eq!(low.temperature, Some(Override::Set(0.0)));
        assert_eq!(low.think, Some(Override::Set(false)));
        assert_eq!(low.max_tokens, Some(Override::Clear));
        low.overlay(&ChatOptionsPatch::default());
        assert_eq!(low.max_tokens, Some(Override::Clear));
        low.overlay(&toml::from_str("max_tokens = 64").unwrap());
        assert_eq!(low.max_tokens, Some(Override::Set(64)));
    }

    #[test]
    fn empty_patch_contains_no_business_defaults() {
        assert!(ChatOptionsPatch::default().is_empty());
        assert!(TtsOptionsPatch::default().is_empty());
        assert!(AsrOptionsPatch::default().is_empty());
        assert!(
            toml::to_string(&ChatOptionsPatch::default())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn text_extraction_options_are_plain_booleans() {
        let patch: ChatOptionsPatch =
            toml::from_str("extract_text_reasoning = false\nextract_text_tool_calls = true")
                .unwrap();
        assert_eq!(patch.extract_text_reasoning, Some(false));
        assert_eq!(patch.extract_text_tool_calls, Some(true));
        for field in ["extract_text_reasoning", "extract_text_tool_calls"] {
            assert!(toml::from_str::<ChatOptionsPatch>(&format!("{field} = \"null\"")).is_err());
        }
    }

    #[test]
    fn text_extraction_is_opt_in_outside_documented_or_prompt_guided_defaults() {
        assert_eq!(
            TextExtraction::default(),
            TextExtraction {
                reasoning: false,
                tool_calls: false
            }
        );
        for (kind, reasoning, tool_calls) in [
            ("openai", false, false),
            ("anthropic", false, false),
            ("deepseek", false, false),
            ("qwen", false, false),
            ("vllm", false, false),
            ("openai-compatible", false, false),
            ("minimax", true, false),
            ("groq", true, true),
            ("ollama", false, true),
            ("hailo_ollama", false, true),
            ("gemini", false, true),
            ("gemini_cli", false, true),
            ("grok_cli", false, true),
            ("kilocli", false, true),
            ("venice", false, true),
            ("atomic_chat", false, true),
            ("custom", false, false),
        ] {
            let registry: crate::provider_registry::ProviderRegistry = toml::from_str(&format!(
                r#"
[connections."provider.prod"]
kind = "{kind}"
[models."chat.main"]
capability = "chat"
provider = "provider.prod"
model = "test-model"
"#
            ))
            .unwrap();
            assert_eq!(
                registry
                    .resolve_chat(&"chat.main".into())
                    .unwrap()
                    .text_extraction(),
                TextExtraction {
                    reasoning,
                    tool_calls
                },
                "{kind}"
            );
        }
    }

    #[test]
    fn native_wire_modes_disable_only_the_implicit_tool_extraction_default() {
        let registry: crate::provider_registry::ProviderRegistry = toml::from_str(
            r#"
[connections."gateway.prod"]
kind = "custom"
uri = "https://example.test/v1"
[connections."gateway.prod".api.chat]

[models."chat.default"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"

[models."chat.native"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"
options = { native_tools = true }

[models."chat.responses"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"
api = { wire_api = "responses" }

[models."chat.explicit"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"
api = { wire_api = "responses" }
options = { extract_text_tool_calls = true }
"#,
        )
        .unwrap();
        for (name, tool_calls) in [
            ("chat.default", false),
            ("chat.native", false),
            ("chat.responses", false),
            ("chat.explicit", true),
        ] {
            assert_eq!(
                registry
                    .resolve_chat(&name.into())
                    .unwrap()
                    .text_extraction(),
                TextExtraction {
                    reasoning: false,
                    tool_calls
                },
                "{name}"
            );
        }
    }

    #[test]
    fn legacy_extraction_defaults_use_the_selected_driver_and_model_options() {
        let mut config = crate::schema::Config::default();
        let provider = config
            .providers
            .models
            .ensure("openai", "proxy.prod")
            .unwrap();
        provider.kind = Some("groq".to_owned());
        assert_eq!(
            crate::migration::admit_test_input(&config).model_text_extraction("openai.proxy.prod"),
            TextExtraction {
                reasoning: true,
                tool_calls: true
            }
        );
        let provider = config
            .providers
            .models
            .ensure("openai", "proxy.prod")
            .unwrap();
        provider.models.insert(
            "native.model".to_owned(),
            crate::schema::ModelEntryConfig {
                native_tools: Some(true),
                ..Default::default()
            },
        );
        assert_eq!(
            crate::migration::admit_test_input(&config).model_text_extraction("openai.proxy.prod"),
            TextExtraction {
                reasoning: true,
                tool_calls: false
            }
        );
        assert_eq!(
            crate::migration::admit_test_input(&config)
                .model_text_extraction("openai.proxy.prod.native.model"),
            TextExtraction {
                reasoning: true,
                tool_calls: false
            }
        );
        assert_eq!(
            config.model_text_extraction("not-configured"),
            TextExtraction::default()
        );
    }

    #[test]
    fn text_extraction_inherits_api_and_preset_then_model_overrides() {
        let registry: crate::provider_registry::ProviderRegistry = toml::from_str(
            r#"
[connections."gateway.prod"]
kind = "custom"
uri = "https://example.test/v1"
[connections."gateway.prod".api.chat.options]
extract_text_reasoning = false
extract_text_tool_calls = false

[model_presets."text.tools"]
capability = "chat"
options = { extract_text_tool_calls = true }

[models."chat.inherited"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"

[models."chat.preset"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"
preset = "text.tools"

[models."chat.override"]
capability = "chat"
provider = "gateway.prod"
model = "shared-model"
preset = "text.tools"
options = { extract_text_reasoning = true, extract_text_tool_calls = false }
"#,
        )
        .unwrap();
        for (name, reasoning, tool_calls) in [
            ("chat.inherited", false, false),
            ("chat.preset", false, true),
            ("chat.override", true, false),
        ] {
            let resolved = registry.resolve_chat(&name.into()).unwrap();
            assert_eq!(resolved.extract_text_reasoning(), reasoning);
            assert_eq!(resolved.extract_text_tool_calls(), tool_calls);
        }
    }
}
