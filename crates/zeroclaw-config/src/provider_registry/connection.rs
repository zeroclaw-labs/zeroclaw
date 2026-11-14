//! Closed connection variants; shared access never broadens driver support.

use super::{AsrOptionsPatch, ChatOptionsPatch, PresetId, TtsOptionsPatch, api::*};
use crate::schema::{
    DecisionWireApi, GlmEndpoint, KiloEndpoint, MinimaxEndpoint, MoonshotEndpoint, QwenEndpoint,
    StepfunEndpoint, ZaiEndpoint,
};
use crate::security::SecretStore;
use crate::traits::{MASKED_SECRET, SecretField};
use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use zeroclaw_api::capability::{Capability, CapabilitySet};

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
    ($name:ident { $($(#[$attr:meta])* $field:ident: $type:ty),* $(,)? }) => {
        #[derive(Clone, Serialize, Deserialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(deny_unknown_fields)]
        pub struct $name {
            #[serde(default)]
            pub capabilities: CapabilitySet,
            $($(#[$attr])* #[serde(default)] pub $field: $type,)*
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
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: OpenAiApi,
});
connection_struct!(OpenAiConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<OpenAiAuth>,
    api: OpenAiApi,
});
connection_struct!(HttpChatConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: ChatDecisionApi,
});
connection_struct!(AnthropicConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: ChatApi,
});
connection_struct!(FixedChatConnection {
    auth: Option<ApiKeyAuth>,
    api: ChatDecisionApi,
});
connection_struct!(QwenConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<QwenAuth>,
    extra: QwenExtra,
    api: ChatApi,
});
connection_struct!(MinimaxConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
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
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: MoonshotExtra,
    api: ChatApi,
});
connection_struct!(StepfunConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: StepfunExtra,
    api: ChatApi,
});
connection_struct!(GlmConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: GlmExtra,
    api: ChatApi,
});
connection_struct!(ZaiConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: ZaiExtra,
    api: ChatApi,
});
connection_struct!(KiloConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    tls_ca_cert_path: Option<String>,
    auth: Option<ApiKeyAuth>,
    extra: KiloExtra,
    api: ChatApi,
});
connection_struct!(GroqConnection {
    uri: Option<String>,
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
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
    #[cfg_attr(feature = "schema-export", schemars(extend("x-secret" = true)))]
    extra_headers: BTreeMap<String, String>,
    auth: Option<ApiKeyAuth>,
    api: HailoApi,
});
connection_struct!(SystemOneConnection {
    uri: Option<String>,
    auth: Option<ApiKeyAuth>,
    api: DecisionApi,
});

macro_rules! provider_connections {
    ($( $variant:ident($type:ty) = $kind:literal [$($capability:ident),*]
        $(decision [$first_wire:ident $(, $other_wire:ident)*])?
    ),* $(,)?) => {
        #[derive(Debug, Clone, Serialize)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(tag = "kind")]
        pub enum ProviderConnection {
            $(#[serde(rename = $kind)] $variant($type),)*
        }

        // Keep the wire enum separate from the public enum so deserialization
        // can inject the kind's documented capability defaults only when the
        // field is omitted. An explicit `capabilities = []` remains an
        // intentional override and is not mistaken for a missing value.
        #[derive(Deserialize)]
        #[serde(tag = "kind")]
        enum ProviderConnectionWire {
            $(#[serde(rename = $kind)] $variant($type),)*
        }

        impl<'de> Deserialize<'de> for ProviderConnection {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let mut value = Value::deserialize(deserializer)?;
                let object = value.as_object_mut().ok_or_else(|| {
                    serde::de::Error::custom("provider connection must be a TOML table")
                })?;
                if !object.contains_key("capabilities") {
                    let kind = object
                        .get("kind")
                        .and_then(Value::as_str)
                        .ok_or_else(|| serde::de::Error::custom("provider connection kind is missing"))?;
                    let defaults: CapabilitySet = match kind {
                        $( $kind => [
                            $(Capability::$capability,)*
                            $({ let _ = DecisionWireApi::$first_wire; Capability::Decision },)?
                        ].into_iter().collect(), )*
                        _ => CapabilitySet::new(),
                    };
                    object.insert(
                        "capabilities".to_owned(),
                        serde_json::to_value(defaults).map_err(serde::de::Error::custom)?,
                    );
                }
                let wire: ProviderConnectionWire =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match wire {
                    $(ProviderConnectionWire::$variant(connection) => Ok(Self::$variant(connection)),)*
                }
            }
        }

        impl ProviderConnection {
            pub const fn kind(&self) -> &'static str { match self { $(Self::$variant(_) => $kind,)* } }
            pub fn capabilities(&self) -> &CapabilitySet { match self { $(Self::$variant(c) => &c.capabilities,)* } }
            /// Capabilities implemented by the concrete adapter. The
            /// configurable `capabilities` set can further narrow this list,
            /// but cannot advertise a protocol the adapter does not provide.
            pub fn implemented_capabilities(&self) -> CapabilitySet {
                match self {
                    $(Self::$variant(_) => [
                        $(Capability::$capability,)*
                        $({ let _ = DecisionWireApi::$first_wire; Capability::Decision },)?
                    ].into_iter().collect(),)*
                }
            }
            pub fn supports(&self, capability: Capability) -> bool {
                self.capabilities().contains(&capability)
                    && self.implemented_capabilities().contains(&capability)
            }
            /// Native decision protocols implemented by this connection kind,
            /// in default preference order. This declaration also determines
            /// whether `decision` is a supported/default capability.
            pub fn decision_wire_apis(&self) -> &'static [DecisionWireApi] {
                match self { $(Self::$variant(_) => &[
                    $(DecisionWireApi::$first_wire, $(DecisionWireApi::$other_wire,)*)?
                ],)* }
            }
            pub fn default_decision_wire_api(&self) -> Option<DecisionWireApi> {
                self.decision_wire_apis().first().copied()
            }
            /// Whether this connection selects the OpenAI Codex OAuth adapter
            /// instead of the ordinary API-key adapter.
            pub fn uses_openai_codex_auth(&self) -> bool {
                match self {
                    Self::OpenAi(connection) => {
                        matches!(connection.auth, Some(OpenAiAuth::Codex {}))
                    }
                    _ => false,
                }
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
            pub fn decision_api(&self) -> Option<&DecisionApiConfig> { match self { $(Self::$variant(c) => <$type as ConnectionSettings>::decision_api(c),)* } }
            /// Command selected by a subprocess TTS connection, when the
            /// connection kind owns one.  Keeping this accessor on the
            /// tagged connection prevents consumers from reconstructing a
            /// second provider lookup table.
            pub fn tts_command(&self) -> Option<&str> { match self { $(Self::$variant(c) => ConnectionSettings::tts_command(c),)* } }
            pub fn configured_apis(&self) -> Vec<Capability> { match self { $(Self::$variant(c) => ConnectionSettings::configured_apis(c),)* } }

        }
    };
}

const CONNECTION_SECRET_KEYS: &[&str] = &["api_key", "refresh_token", "client_secret"];

fn is_connection_secret_key(key: &str) -> bool {
    CONNECTION_SECRET_KEYS.contains(&key)
}

fn is_connection_secret_map(key: &str) -> bool {
    // Match legacy ModelProviderConfig: every custom header is a secret,
    // including application-specific auth headers whose names we cannot know.
    key == "extra_headers"
}

fn visit_connection_secrets(
    value: &mut Value,
    path: &str,
    secret_map: bool,
    visit: &mut impl FnMut(&mut String, &str) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.starts_with('/') {
                    format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"))
                } else {
                    format!("{path}.{key}")
                };
                if secret_map || is_connection_secret_key(key) {
                    if let Value::String(secret) = child {
                        visit(secret, &child_path)?;
                    }
                } else {
                    visit_connection_secrets(
                        child,
                        &child_path,
                        is_connection_secret_map(key),
                        visit,
                    )?;
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                let child_path = if path.starts_with('/') {
                    format!("{path}/{index}")
                } else {
                    format!("{path}[{index}]")
                };
                visit_connection_secrets(child, &child_path, false, visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn mask_connection_secrets(value: &mut Value, secret_map: bool) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if secret_map || is_connection_secret_key(key) {
                    if let Value::String(secret) = child
                        && !secret.is_empty()
                    {
                        *secret = MASKED_SECRET.to_owned();
                    }
                } else {
                    mask_connection_secrets(child, is_connection_secret_map(key));
                }
            }
        }
        Value::Array(items) => {
            for child in items {
                mask_connection_secrets(child, false);
            }
        }
        _ => {}
    }
}

fn restore_connection_secrets(value: &mut Value, current: &Value, secret_map: bool) {
    match (value, current) {
        (Value::Object(object), Value::Object(current_object)) => {
            for (key, child) in object {
                let Some(current_child) = current_object.get(key) else {
                    continue;
                };
                if secret_map || is_connection_secret_key(key) {
                    if let (Value::String(secret), Value::String(current_secret)) =
                        (child, current_child)
                        && secret == MASKED_SECRET
                    {
                        *secret = current_secret.clone();
                    }
                } else {
                    restore_connection_secrets(child, current_child, is_connection_secret_map(key));
                }
            }
        }
        (Value::Array(items), Value::Array(current_items)) => {
            for (child, current_child) in items.iter_mut().zip(current_items) {
                restore_connection_secrets(child, current_child, false);
            }
        }
        _ => {}
    }
}

impl ProviderConnection {
    pub(super) fn with_secret_json_for_editor(
        &mut self,
        mut visit: impl FnMut(&str) -> anyhow::Result<()>,
    ) -> anyhow::Result<()> {
        self.with_secret_json(|secret, _| visit(secret), "providers.connections")
    }

    fn with_secret_json(
        &mut self,
        mut visit: impl FnMut(&mut String, &str) -> anyhow::Result<()>,
        field: &str,
    ) -> anyhow::Result<()> {
        let mut value = serde_json::to_value(&*self).context("serialize provider connection")?;
        visit_connection_secrets(&mut value, field, false, &mut visit)?;
        *self = serde_json::from_value(value).context("deserialize provider connection")?;
        Ok(())
    }

    /// Collect external references through the same secret-field traversal as
    /// encryption. The caller supplies an escaped JSON pointer for exact keys.
    pub(crate) fn onepassword_references(
        &self,
        pointer: &str,
    ) -> anyhow::Result<std::collections::HashMap<String, String>> {
        let mut value = serde_json::to_value(self).context("serialize provider secrets")?;
        let mut references = std::collections::HashMap::new();
        visit_connection_secrets(&mut value, pointer, false, &mut |secret, path| {
            if SecretStore::is_onepassword_ref(secret) {
                references.insert(path.to_owned(), secret.clone());
            }
            Ok(())
        })?;
        Ok(references)
    }

    pub fn encrypt_secrets(&mut self, store: &SecretStore, field: &str) -> anyhow::Result<()> {
        self.with_secret_json(|secret, path| secret.encrypt_in_place(store, path), field)
    }

    pub fn decrypt_secrets(&mut self, store: &SecretStore, field: &str) -> anyhow::Result<()> {
        self.with_secret_json(|secret, path| secret.decrypt_in_place(store, path), field)
    }

    pub fn mask_secrets(&mut self) {
        let Ok(mut value) = serde_json::to_value(&*self) else {
            return;
        };
        mask_connection_secrets(&mut value, false);
        if let Ok(updated) = serde_json::from_value(value) {
            *self = updated;
        }
    }

    pub fn restore_secrets_from(&mut self, current: &Self) {
        let (Ok(mut value), Ok(current_value)) =
            (serde_json::to_value(&*self), serde_json::to_value(current))
        else {
            return;
        };
        restore_connection_secrets(&mut value, &current_value, false);
        if let Ok(updated) = serde_json::from_value(value) {
            *self = updated;
        }
    }

    pub fn has_secrets(&self) -> bool {
        fn contains(value: &Value, secret_map: bool) -> bool {
            match value {
                Value::Object(object) => object.iter().any(|(key, child)| {
                    ((secret_map || is_connection_secret_key(key))
                        && child.as_str().is_some_and(|secret| !secret.is_empty()))
                        || (!secret_map
                            && !is_connection_secret_key(key)
                            && contains(child, is_connection_secret_map(key)))
                }),
                Value::Array(items) => items.iter().any(|item| contains(item, false)),
                _ => false,
            }
        }
        serde_json::to_value(self).is_ok_and(|value| contains(&value, false))
    }
}
provider_connections! {
    OpenAi(OpenAiConnection) = "openai" [Chat, Embedding, Tts, Asr] decision [OpenAiDecisions],
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
    OpenRouter(FixedChatConnection) = "openrouter" [Chat, Embedding] decision [SystemOne],
    Ollama(HttpChatConnection) = "ollama" [Chat] decision [SystemOne],
    HailoOllama(HailoConnection) = "hailo_ollama" [Chat],
    Gemini(GeminiConnection) = "gemini" [Chat],
    GeminiCli(CliConnection) = "gemini_cli" [Chat],
    GrokCli(GrokConnection) = "grok_cli" [Chat],
    Bedrock(BedrockConnection) = "bedrock" [Chat],
    Telnyx(FixedChatConnection) = "telnyx" [Chat],
    Together(HttpChatConnection) = "together" [Chat],
    Fireworks(HttpChatConnection) = "fireworks" [Chat] decision [OpenAiDecisions, SystemOne],
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
    Lmstudio(HttpChatConnection) = "lmstudio" [Chat] decision [OpenAiDecisions, SystemOne],
    Llamacpp(HttpChatConnection) = "llamacpp" [Chat] decision [SystemOne],
    Sglang(HttpChatConnection) = "sglang" [Chat] decision [SystemOne],
    Vllm(HttpChatConnection) = "vllm" [Chat, Embedding, Asr] decision [OpenAiDecisions, SystemOne],
    Osaurus(HttpChatConnection) = "osaurus" [Chat],
    Litellm(HttpChatConnection) = "litellm" [Chat] decision [OpenAiDecisions, SystemOne],
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
    Custom(CompatibleConnection) = "custom" [Chat, Embedding, Tts, Asr] decision [OpenAiDecisions, SystemOne],
    OpenAiCompatible(CompatibleConnection) = "openai-compatible" [Chat, Embedding, Tts, Asr] decision [OpenAiDecisions, SystemOne],
    ElevenLabs(VoiceConnection) = "elevenlabs" [Tts],
    Google(GoogleConnection) = "google" [Tts, Asr],
    Deepgram(AsrConnection) = "deepgram" [Asr],
    AssemblyAi(AsrConnection) = "assemblyai" [Asr],
    LocalWhisper(LocalWhisperConnection) = "local_whisper" [Asr],
    Piper(PiperConnection) = "piper" [Tts],
    Edge(EdgeConnection) = "edge" [Tts],
    SystemOne(SystemOneConnection) = "system-one" [] decision [SystemOne],
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
        self.api.decision.as_ref()
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
        if self.api.decision.is_some() {
            out.push(Capability::Decision);
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
        self.api.decision.as_ref()
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
        if self.api.decision.is_some() {
            out.push(Capability::Decision);
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
        self.api.decision.as_ref()
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
        if self.api.decision.is_some() {
            out.push(Capability::Decision);
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
        self.api.decision.as_ref()
    }
    fn configured_apis(&self) -> Vec<Capability> {
        let mut out = Vec::new();
        if self.api.chat.is_some() {
            out.push(Capability::Chat);
        }
        if self.api.decision.is_some() {
            out.push(Capability::Decision);
        }
        out
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
    fn decision_api(&self) -> Option<&DecisionApiConfig> {
        self.api.decision.as_ref()
    }
    fn configured_apis(&self) -> Vec<Capability> {
        if self.api.decision.is_some() {
            vec![Capability::Decision]
        } else {
            Vec::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_capabilities_use_kind_defaults_and_explicit_values_win() {
        let custom: ProviderConnection = toml::from_str(
            r#"
kind = "custom"
uri = "https://gateway.example/v1"
"#,
        )
        .expect("custom connection should use its default capabilities");
        assert_eq!(
            custom.capabilities(),
            &[
                Capability::Chat,
                Capability::Embedding,
                Capability::Tts,
                Capability::Asr,
                Capability::Decision,
            ]
            .into_iter()
            .collect()
        );

        let narrowed: ProviderConnection = toml::from_str(
            r#"
kind = "custom"
capabilities = ["chat"]
"#,
        )
        .expect("explicit capability overrides should be preserved");
        assert_eq!(
            narrowed.capabilities(),
            &[Capability::Chat].into_iter().collect()
        );

        let openai: ProviderConnection = toml::from_str(r#"kind = "openai""#)
            .expect("openai connection should use its documented capability defaults");
        assert_eq!(
            openai.capabilities(),
            &[
                Capability::Chat,
                Capability::Embedding,
                Capability::Tts,
                Capability::Asr,
                Capability::Decision,
            ]
            .into_iter()
            .collect()
        );

        let disabled: ProviderConnection = toml::from_str(
            r#"
kind = "openai"
capabilities = []
"#,
        )
        .expect("an explicit empty capability list should be accepted");
        assert!(disabled.capabilities().is_empty());
    }

    #[test]
    fn decision_defaults_are_native_protocols_per_connection_kind() {
        let openai: ProviderConnection = toml::from_str(r#"kind = "openai""#).unwrap();
        assert_eq!(
            openai.decision_wire_apis(),
            &[DecisionWireApi::OpenAiDecisions]
        );

        let vllm: ProviderConnection = toml::from_str(r#"kind = "vllm""#).unwrap();
        assert_eq!(
            vllm.decision_wire_apis(),
            &[DecisionWireApi::OpenAiDecisions, DecisionWireApi::SystemOne]
        );

        let sglang: ProviderConnection = toml::from_str(r#"kind = "sglang""#).unwrap();
        assert_eq!(sglang.decision_wire_apis(), &[DecisionWireApi::SystemOne]);

        let chat_only: ProviderConnection = toml::from_str(r#"kind = "mistral""#).unwrap();
        assert!(chat_only.decision_wire_apis().is_empty());
    }
}
