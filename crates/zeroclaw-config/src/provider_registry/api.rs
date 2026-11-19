use std::num::NonZeroU64;

use serde::{Deserialize, Serialize};

use super::{AsrOptionsPatch, ChatOptionsPatch, Override, PresetId, TtsOptionsPatch};
use crate::schema::{DecisionWireApi, WireApi};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct ChatApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<WireApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_timeout_secs: Option<Override<NonZeroU64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "ChatOptionsPatch::is_empty")]
    pub options: ChatOptionsPatch,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub live_pricing: Option<bool>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct ChatApiPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<WireApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub read_timeout_secs: Option<Override<NonZeroU64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_timeout_secs: Option<NonZeroU64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct TtsApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "TtsOptionsPatch::is_empty")]
    pub options: TtsOptionsPatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct EdgeTtsApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "TtsOptionsPatch::is_empty")]
    pub options: TtsOptionsPatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct AsrApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "AsrOptionsPatch::is_empty")]
    pub options: AsrOptionsPatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct LocalWhisperApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_audio_bytes: Option<usize>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct AsrApiPatch {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct DecisionApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire_api: Option<DecisionWireApi>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_timeout_secs: Option<Override<NonZeroU64>>,
}

/// Model-level decision settings have the same shape as connection defaults;
/// the resolver applies them as a narrow override at admission time.
pub type DecisionApiPatch = DecisionApiConfig;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct CliChatApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "ChatOptionsPatch::is_empty")]
    pub options: ChatOptionsPatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct GrokChatApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary_path: Option<String>,
    pub working_directory: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env_passthrough: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_acp_stdout_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "ChatOptionsPatch::is_empty")]
    pub options: ChatOptionsPatch,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(default, deny_unknown_fields)]
pub struct HailoChatApiConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_timeout_secs: Option<NonZeroU64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preset: Option<PresetId>,
    #[serde(skip_serializing_if = "ChatOptionsPatch::is_empty")]
    pub options: ChatOptionsPatch,
}

// Capability tables are closed. Embedding has no API object because its wire
// shape is fixed. Decision has an API object because the same capability can
// use different native protocols (OpenAI Decisions and System One).
macro_rules! api_tables {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(default, deny_unknown_fields)]
        pub struct $name {
            $(#[serde(skip_serializing_if = "Option::is_none")]
            pub $field: Option<$type>,)*
        }
    };
}

api_tables!(ChatApi {
    chat: ChatApiConfig
});
api_tables!(ChatDecisionApi {
    chat: ChatApiConfig,
    decision: DecisionApiConfig,
    tts: TtsApiConfig,
    asr: AsrApiConfig
});
api_tables!(OpenAiApi {
    chat: ChatApiConfig,
    tts: TtsApiConfig,
    asr: AsrApiConfig,
    decision: DecisionApiConfig
});
api_tables!(VoiceApi { tts: TtsApiConfig });
api_tables!(GoogleApi {
    tts: TtsApiConfig,
    asr: AsrApiConfig
});
api_tables!(GroqApi {
    chat: ChatApiConfig,
    asr: AsrApiConfig
});
api_tables!(DecisionApi {
    decision: DecisionApiConfig
});
api_tables!(AsrApi { asr: AsrApiConfig });
api_tables!(WhisperApi {
    asr: LocalWhisperApiConfig
});
api_tables!(EdgeApi {
    tts: EdgeTtsApiConfig
});
api_tables!(CliApi {
    chat: CliChatApiConfig
});
api_tables!(GrokApi {
    chat: GrokChatApiConfig
});
api_tables!(HailoApi {
    chat: HailoChatApiConfig
});
