//! Model identity and per-model policy. Connection-specific input is decoded
//! only after looking up the referenced connection in the candidate registry.

use serde::{Deserialize, Serialize};

use super::{
    AsrApiPatch, AsrOptionsPatch, ChatApiPatch, ChatOptionsPatch, DecisionApiPatch, PresetId,
    ProviderConnection, ProviderId, TtsOptionsPatch,
};
use crate::schema::{ModelCostRates, TranscriptionCostRates, TtsCostRates};
use zeroclaw_api::capability::Capability;

macro_rules! metadata {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            pub $field: Option<$type>,)*
        }
        impl $name {
            pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*];
        }
    };
}

metadata!(ChatMetadata {
    context_window: usize,
    vision: bool,
    pricing: ModelCostRates,
});
metadata!(EmbeddingMetadata { dimensions: usize });
metadata!(TtsMetadata {
    pricing: TtsCostRates
});
metadata!(AsrMetadata {
    pricing: TranscriptionCostRates
});

/// The generic parameter only changes the input representation of `extra`.
/// All ordinary model fields have one definition for input and canonical state.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields, bound(deserialize = "E: Deserialize<'de>"))]
pub struct ChatModelConfig<E = ChatModelExtra> {
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<ChatApiPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra: Option<E>,
    #[serde(default, skip_serializing_if = "ChatOptionsPatch::is_empty")]
    pub options: ChatOptionsPatch,
    #[serde(default)]
    pub metadata: ChatMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct EmbeddingModelConfig {
    pub provider: ProviderId,
    pub model: String,
    #[serde(default)]
    pub metadata: EmbeddingMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct TtsModelConfig {
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(default, skip_serializing_if = "TtsOptionsPatch::is_empty")]
    pub options: TtsOptionsPatch,
    #[serde(default)]
    pub metadata: TtsMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AsrModelConfig {
    pub provider: ProviderId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<AsrApiPatch>,
    #[serde(default, skip_serializing_if = "AsrOptionsPatch::is_empty")]
    pub options: AsrOptionsPatch,
    #[serde(default)]
    pub metadata: AsrMetadata,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct DecisionModelConfig {
    pub provider: ProviderId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api: Option<DecisionApiPatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(
    tag = "capability",
    rename_all = "lowercase",
    bound(deserialize = "E: Deserialize<'de>")
)]
pub enum ModelDefinition<E = ChatModelExtra> {
    Chat(ChatModelConfig<E>),
    Embedding(EmbeddingModelConfig),
    Tts(TtsModelConfig),
    Asr(AsrModelConfig),
    Decision(DecisionModelConfig),
}

impl<E> ModelDefinition<E> {
    pub const fn capability(&self) -> Capability {
        match self {
            Self::Chat(_) => Capability::Chat,
            Self::Embedding(_) => Capability::Embedding,
            Self::Tts(_) => Capability::Tts,
            Self::Asr(_) => Capability::Asr,
            Self::Decision(_) => Capability::Decision,
        }
    }

    pub fn provider(&self) -> &ProviderId {
        match self {
            Self::Chat(c) => &c.provider,
            Self::Embedding(c) => &c.provider,
            Self::Tts(c) => &c.provider,
            Self::Asr(c) => &c.provider,
            Self::Decision(c) => &c.provider,
        }
    }

    pub fn upstream_model(&self) -> Option<&str> {
        match self {
            Self::Chat(c) => c.model.as_deref(),
            Self::Embedding(c) => Some(&c.model),
            Self::Tts(c) => c.model.as_deref(),
            Self::Asr(c) => c.model.as_deref(),
            Self::Decision(c) => Some(&c.model),
        }
    }
}

/// No `Deserialize`: callers must associate the input with its connection.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(untagged)]
pub enum ChatModelExtra {
    Azure(AzureChatModelExtra),
    Anthropic(AnthropicChatModelExtra),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AzureChatModelExtra {
    pub deployment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(deny_unknown_fields)]
pub struct AnthropicChatModelExtra {
    pub server_fallback_models: Vec<String>,
}

impl ModelDefinition<serde_json::Value> {
    pub(super) fn associate(
        self,
        connection: &ProviderConnection,
    ) -> Result<ModelDefinition, serde_json::Error> {
        Ok(match self {
            Self::Chat(c) => {
                let extra = c
                    .extra
                    .map(|raw| match connection {
                        ProviderConnection::Azure(_) => {
                            serde_json::from_value(raw).map(ChatModelExtra::Azure)
                        }
                        ProviderConnection::Anthropic(_) => {
                            serde_json::from_value(raw).map(ChatModelExtra::Anthropic)
                        }
                        _ => Err(<serde_json::Error as serde::de::Error>::custom(
                            "extra is not supported by this chat connection kind",
                        )),
                    })
                    .transpose()?;
                ModelDefinition::Chat(ChatModelConfig {
                    provider: c.provider,
                    model: c.model,
                    api: c.api,
                    preset: c.preset,
                    extra,
                    options: c.options,
                    metadata: c.metadata,
                })
            }
            Self::Embedding(c) => ModelDefinition::Embedding(c),
            Self::Tts(c) => ModelDefinition::Tts(c),
            Self::Asr(c) => ModelDefinition::Asr(c),
            Self::Decision(c) => ModelDefinition::Decision(c),
        })
    }
}
