//! Closed connection variants; shared access never broadens driver support.

use super::{
    AsrOptionsPatch, Capability, CapabilitySet, ChatOptionsPatch, PresetId, TtsOptionsPatch, api::*,
};
use crate::schema::{
    GlmEndpoint, KiloEndpoint, MinimaxEndpoint, MoonshotEndpoint, QwenEndpoint, StepfunEndpoint,
    ZaiEndpoint,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ApiKeyAuth {
    #[serde(rename = "api_key")]
    ApiKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        api_key: Option<String>,
    },
}
impl std::fmt::Debug for ApiKeyAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApiKeyAuth").finish_non_exhaustive()
    }
}
impl ApiKeyAuth {
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey { api_key } => api_key.as_deref(),
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum OpenAiAuth {
    #[serde(rename = "api_key")]
    ApiKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        api_key: Option<String>,
    },
    #[serde(rename = "codex")]
    Codex {},
}
impl std::fmt::Debug for OpenAiAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiAuth").finish_non_exhaustive()
    }
}
impl OpenAiAuth {
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey { api_key } => api_key.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum QwenAuth {
    #[serde(rename = "api_key")]
    ApiKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        api_key: Option<String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        refresh_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
    },
}
impl std::fmt::Debug for QwenAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QwenAuth").finish_non_exhaustive()
    }
}
impl QwenAuth {
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey { api_key } => api_key.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum GeminiAuth {
    #[serde(rename = "api_key")]
    ApiKey {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        api_key: Option<String>,
    },
    #[serde(rename = "oauth")]
    OAuth {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        client_id: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
        client_secret: Option<String>,
    },
}
impl std::fmt::Debug for GeminiAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GeminiAuth").finish_non_exhaustive()
    }
}
impl GeminiAuth {
    pub fn api_key(&self) -> Option<&str> {
        match self {
            Self::ApiKey { api_key } => api_key.as_deref(),
            _ => None,
        }
    }
}

pub type MinimaxAuth = QwenAuth;

macro_rules! strict_struct {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Debug, Clone, Default, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(default, deny_unknown_fields)]
        pub struct $name { $(pub $field: $type,)* }
        impl $name { pub const FIELDS: &'static [&'static str] = &[$(stringify!($field)),*]; }
    };
}
strict_struct!(AzureExtra { resource: String, api_version: Option<String> });
strict_struct!(GeminiExtra { project: Option<String> });
strict_struct!(BedrockExtra { region: Option<String> });
strict_struct!(QwenExtra {
    endpoint: QwenEndpoint
});
strict_struct!(MoonshotExtra {
    endpoint: MoonshotEndpoint
});
strict_struct!(StepfunExtra {
    endpoint: StepfunEndpoint
});
strict_struct!(GlmExtra {
    endpoint: GlmEndpoint
});
strict_struct!(MinimaxExtra {
    endpoint: MinimaxEndpoint
});
strict_struct!(ZaiExtra {
    endpoint: ZaiEndpoint
});
strict_struct!(KiloExtra {
    endpoint: KiloEndpoint
});

macro_rules! connection_struct {
    ($name:ident { $($field:ident: $type:ty),* $(,)? }) => {
        #[derive(Clone, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            pub capabilities: CapabilitySet,
            $(#[serde(default)] pub $field: $type,)*
        }
        impl $name { pub const FIELDS: &'static [&'static str] = &["kind", "capabilities", $(stringify!($field)),*]; }
        impl std::fmt::Debug for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct(stringify!($name)).field("capabilities", &self.capabilities).finish_non_exhaustive()
            }
        }
    };
}
connection_struct!(CompatibleConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: OpenAiApi,
});
connection_struct!(OpenAiConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<OpenAiAuth>,
    api: OpenAiApi,
});
connection_struct!(HttpChatConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: ChatApi,
});
connection_struct!(AnthropicConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: ChatApi,
});
connection_struct!(FixedChatConnection {
    auth: Option<ApiKeyAuth>,
    api: ChatApi,
});
connection_struct!(QwenConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<QwenAuth>,
    extra: QwenExtra,
    api: ChatApi,
});
connection_struct!(MinimaxConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<MinimaxAuth>,
    extra: MinimaxExtra,
    api: ChatApi,
});
connection_struct!(GeminiConnection {
    uri: Option<String>,
    auth: Option<GeminiAuth>,
    extra: GeminiExtra,
    api: ChatApi,
});
connection_struct!(AzureConnection {
    auth: Option<ApiKeyAuth>,
    extra: AzureExtra,
    api: ChatApi,
});
connection_struct!(BedrockConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: BedrockExtra,
    api: ChatApi,
});
connection_struct!(MoonshotConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: MoonshotExtra,
    api: ChatApi,
});
connection_struct!(StepfunConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: StepfunExtra,
    api: ChatApi,
});
connection_struct!(GlmConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: GlmExtra,
    api: ChatApi,
});
connection_struct!(ZaiConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: ZaiExtra,
    api: ChatApi,
});
connection_struct!(KiloConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: KiloExtra,
    api: ChatApi,
});
connection_struct!(GroqConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: GroqApi,
});
connection_struct!(VoiceConnection {
    auth: Option<ApiKeyAuth>,
    api: VoiceApi,
});
connection_struct!(GoogleConnection {
    auth: Option<ApiKeyAuth>,
    api: GoogleApi,
});
connection_struct!(AsrConnection {
    auth: Option<ApiKeyAuth>,
    api: AsrApi,
});
connection_struct!(LocalWhisperConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: WhisperApi,
});
connection_struct!(PiperConnection {
    uri: Option<String>,
    api: VoiceApi,
});
connection_struct!(EdgeConnection { api: EdgeApi });
connection_struct!(CliConnection { api: CliApi });
connection_struct!(GrokConnection { api: GrokApi });
connection_struct!(HailoConnection {
    uri: Option<String>,
    extra_headers: BTreeMap<String, String>,
    auth: Option<ApiKeyAuth>,
    api: HailoApi,
});
connection_struct!(SystemOneConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
});

macro_rules! provider_connections {
    ($( $variant:ident($type:ty) = $kind:literal [$($capability:ident),*] ),* $(,)?) => {
        #[derive(Debug, Clone, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(tag = "kind")]
        pub enum ProviderConnection {
            $(#[serde(rename = $kind)] $variant($type),)*
        }
        impl ProviderConnection {
            pub const fn kind(&self) -> &'static str { match self { $(Self::$variant(_) => $kind,)* } }
            pub fn capabilities(&self) -> &CapabilitySet { match self { $(Self::$variant(c) => &c.capabilities,)* } }
            pub fn supports(&self, capability: Capability) -> bool {
                match self { $(Self::$variant(_) => [$(Capability::$capability),*].contains(&capability),)* }
            }
            pub fn accepts_connection_field(&self, name: &str) -> bool {
                match self { $(Self::$variant(_) => <$type>::FIELDS.contains(&name),)* }
            }
            pub fn uri(&self) -> Option<&str> { match self { $(Self::$variant(c) => ConnectionSettings::uri(c),)* } }
            pub fn api_key(&self) -> Option<&str> { match self { $(Self::$variant(c) => ConnectionSettings::api_key(c),)* } }
            pub fn extra_headers(&self) -> Option<&BTreeMap<String, String>> { match self { $(Self::$variant(c) => ConnectionSettings::extra_headers(c),)* } }
            pub fn chat_api(&self) -> Option<&ChatApiConfig> { match self { $(Self::$variant(c) => ConnectionSettings::chat_api(c),)* } }
            pub fn tls_ca_cert_path(&self) -> Option<&str> { match self { $(Self::$variant(c) => ConnectionSettings::tls_ca_cert_path(c),)* } }
            pub fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> { match self { $(Self::$variant(c) => ConnectionSettings::chat_defaults(c),)* } }
            pub fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> { match self { $(Self::$variant(c) => ConnectionSettings::tts_defaults(c),)* } }
            pub fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> { match self { $(Self::$variant(c) => ConnectionSettings::asr_defaults(c),)* } }
            /// Command selected by a subprocess TTS connection, when the
            /// connection kind owns one.  Keeping this accessor on the
            /// tagged connection prevents consumers from reconstructing a
            /// second provider lookup table.
            pub fn tts_command(&self) -> Option<&str> { match self { $(Self::$variant(c) => ConnectionSettings::tts_command(c),)* } }
            pub fn configured_apis(&self) -> Vec<Capability> { match self { $(Self::$variant(c) => ConnectionSettings::configured_apis(c),)* } }

        }
    };
}
provider_connections! {
    OpenAi(OpenAiConnection) = "openai" [Chat, Embedding, Tts, Asr],
    Azure(AzureConnection) = "azure" [Chat],
    Anthropic(AnthropicConnection) = "anthropic" [Chat],
    Moonshot(MoonshotConnection) = "moonshot" [Chat],
    Qwen(QwenConnection) = "qwen" [Chat],
    Glm(GlmConnection) = "glm" [Chat],
    Minimax(MinimaxConnection) = "minimax" [Chat],
    Zai(ZaiConnection) = "zai" [Chat],
    Doubao(HttpChatConnection) = "doubao" [Chat],
    Yi(HttpChatConnection) = "yi" [Chat],
    Hunyuan(HttpChatConnection) = "hunyuan" [Chat],
    Qianfan(HttpChatConnection) = "qianfan" [Chat],
    Baichuan(HttpChatConnection) = "baichuan" [Chat],
    OpenRouter(FixedChatConnection) = "openrouter" [Chat, Embedding],
    Ollama(HttpChatConnection) = "ollama" [Chat],
    HailoOllama(HailoConnection) = "hailo_ollama" [Chat],
    Gemini(GeminiConnection) = "gemini" [Chat],
    GeminiCli(CliConnection) = "gemini_cli" [Chat],
    GrokCli(GrokConnection) = "grok_cli" [Chat],
    Bedrock(BedrockConnection) = "bedrock" [Chat],
    Telnyx(FixedChatConnection) = "telnyx" [Chat],
    Together(HttpChatConnection) = "together" [Chat],
    Fireworks(HttpChatConnection) = "fireworks" [Chat],
    Groq(GroqConnection) = "groq" [Chat, Asr],
    Mistral(HttpChatConnection) = "mistral" [Chat],
    Deepseek(HttpChatConnection) = "deepseek" [Chat],
    AtomicChat(HttpChatConnection) = "atomic_chat" [Chat],
    Cohere(HttpChatConnection) = "cohere" [Chat],
    Perplexity(HttpChatConnection) = "perplexity" [Chat],
    Xai(HttpChatConnection) = "xai" [Chat],
    Cerebras(HttpChatConnection) = "cerebras" [Chat],
    Crusoe(HttpChatConnection) = "crusoe" [Chat],
    Sambanova(HttpChatConnection) = "sambanova" [Chat],
    Hyperbolic(HttpChatConnection) = "hyperbolic" [Chat],
    Deepinfra(HttpChatConnection) = "deepinfra" [Chat],
    Huggingface(HttpChatConnection) = "huggingface" [Chat],
    Ai21(HttpChatConnection) = "ai21" [Chat],
    Reka(HttpChatConnection) = "reka" [Chat],
    Baseten(HttpChatConnection) = "baseten" [Chat],
    Nscale(HttpChatConnection) = "nscale" [Chat],
    Anyscale(HttpChatConnection) = "anyscale" [Chat],
    Nebius(HttpChatConnection) = "nebius" [Chat],
    Friendli(HttpChatConnection) = "friendli" [Chat],
    Stepfun(StepfunConnection) = "stepfun" [Chat],
    Aihubmix(HttpChatConnection) = "aihubmix" [Chat],
    Siliconflow(HttpChatConnection) = "siliconflow" [Chat],
    Astrai(HttpChatConnection) = "astrai" [Chat],
    Avian(HttpChatConnection) = "avian" [Chat],
    Deepmyst(HttpChatConnection) = "deepmyst" [Chat],
    Venice(HttpChatConnection) = "venice" [Chat],
    Nearai(HttpChatConnection) = "nearai" [Chat],
    Novita(HttpChatConnection) = "novita" [Chat],
    Nvidia(HttpChatConnection) = "nvidia" [Chat],
    Vercel(HttpChatConnection) = "vercel" [Chat],
    Cloudflare(HttpChatConnection) = "cloudflare" [Chat],
    AtlasCloud(HttpChatConnection) = "atlascloud" [Chat],
    Ovh(FixedChatConnection) = "ovh" [Chat],
    Copilot(FixedChatConnection) = "copilot" [Chat],
    Lmstudio(HttpChatConnection) = "lmstudio" [Chat],
    Llamacpp(HttpChatConnection) = "llamacpp" [Chat],
    Sglang(HttpChatConnection) = "sglang" [Chat],
    Vllm(HttpChatConnection) = "vllm" [Chat],
    Osaurus(HttpChatConnection) = "osaurus" [Chat],
    Litellm(HttpChatConnection) = "litellm" [Chat],
    Lepton(HttpChatConnection) = "lepton" [Chat],
    Manifest(HttpChatConnection) = "manifest" [Chat],
    Morph(HttpChatConnection) = "morph" [Chat],
    GithubModels(HttpChatConnection) = "github_models" [Chat],
    Upstage(HttpChatConnection) = "upstage" [Chat],
    Featherless(HttpChatConnection) = "featherless" [Chat],
    Arcee(HttpChatConnection) = "arcee" [Chat],
    LambdaAi(HttpChatConnection) = "lambda_ai" [Chat],
    Inception(HttpChatConnection) = "inception" [Chat],
    Synthetic(HttpChatConnection) = "synthetic" [Chat],
    Opencode(HttpChatConnection) = "opencode" [Chat],
    KiloCli(CliConnection) = "kilocli" [Chat],
    Kilo(KiloConnection) = "kilo" [Chat],
    Zerorouter(HttpChatConnection) = "zerorouter" [Chat],
    Custom(CompatibleConnection) = "custom" [Chat, Embedding, Tts, Asr],
    OpenAiCompatible(CompatibleConnection) = "openai-compatible" [Chat, Embedding, Tts, Asr],
    ElevenLabs(VoiceConnection) = "elevenlabs" [Tts],
    Google(GoogleConnection) = "google" [Tts, Asr],
    Deepgram(AsrConnection) = "deepgram" [Asr],
    AssemblyAi(AsrConnection) = "assemblyai" [Asr],
    LocalWhisper(LocalWhisperConnection) = "local_whisper" [Asr],
    Piper(PiperConnection) = "piper" [Tts],
    Edge(EdgeConnection) = "edge" [Tts],
    SystemOne(SystemOneConnection) = "system-one" [Decision],
}

trait ConnectionSettings {
    fn uri(&self) -> Option<&str> {
        None
    }
    fn api_key(&self) -> Option<&str> {
        None
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        None
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        None
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        None
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        None
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        None
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        None
    }
    fn tts_command(&self) -> Option<&str> {
        None
    }
    fn configured_apis(&self) -> Vec<Capability> {
        Vec::new()
    }
}
impl ConnectionSettings for CompatibleConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        self.api
            .asr
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for OpenAiConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        self.api
            .asr
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for HttpChatConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for FixedChatConnection {
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn configured_apis(&self) -> Vec<Capability> {
        if self.api.chat.is_some() {
            vec![Capability::Chat]
        } else {
            Vec::new()
        }
    }
}
impl ConnectionSettings for AnthropicConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for QwenConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for MinimaxConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for GeminiConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for AzureConnection {
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for BedrockConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for MoonshotConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for StepfunConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for GlmConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for ZaiConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for KiloConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for GroqConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_api(&self) -> Option<&ChatApiConfig> {
        self.api.chat.as_ref()
    }
    fn tls_ca_cert_path(&self) -> Option<&str> {
        self.tls_ca_cert_path.as_deref()
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        self.api
            .asr
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for VoiceConnection {
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        out
    }
}
impl ConnectionSettings for GoogleConnection {
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        self.api
            .asr
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for AsrConnection {
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn asr_defaults(&self) -> Option<(Option<&PresetId>, &AsrOptionsPatch)> {
        self.api
            .asr
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for LocalWhisperConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.asr.is_some() {
            out.push(Capability::Asr);
        }
        out
    }
}
impl ConnectionSettings for PiperConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        out
    }
}
impl ConnectionSettings for EdgeConnection {
    fn tts_command(&self) -> Option<&str> {
        self.api.tts.as_ref().and_then(|api| api.command.as_deref())
    }
    fn tts_defaults(&self) -> Option<(Option<&PresetId>, &TtsOptionsPatch)> {
        self.api
            .tts
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.tts.is_some() {
            out.push(Capability::Tts);
        }
        out
    }
}
impl ConnectionSettings for CliConnection {
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for GrokConnection {
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for HailoConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
    fn extra_headers(&self) -> Option<&BTreeMap<String, String>> {
        Some(&self.extra_headers)
    }
    fn chat_defaults(&self) -> Option<(Option<&PresetId>, &ChatOptionsPatch)> {
        self.api
            .chat
            .as_ref()
            .map(|api| (api.preset.as_ref(), &api.options))
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        out
    }
}
impl ConnectionSettings for SystemOneConnection {
    fn uri(&self) -> Option<&str> {
        self.uri.as_deref()
    }
    fn api_key(&self) -> Option<&str> {
        self.auth.as_ref().and_then(|auth| auth.api_key())
    }
}
