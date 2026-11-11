use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use zeroclaw_macros::Configurable;

use crate::secrets::SecretStore;
use crate::traits::SecretFieldInfo;

use super::provider_registry::{
    ModelDefinition, ModelPreset, ModelRef, ProviderConnection, ProviderId, ProviderRegistry,
};
use super::schema::{
    Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnthropicModelProviderConfig,
    AnyscaleModelProviderConfig, ArceeModelProviderConfig, AstraiModelProviderConfig,
    AtlasCloudModelProviderConfig, AtomicChatModelProviderConfig, AvianModelProviderConfig,
    AzureModelProviderConfig, BaichuanModelProviderConfig, BasetenModelProviderConfig,
    BedrockModelProviderConfig, CerebrasModelProviderConfig, CloudflareModelProviderConfig,
    CohereModelProviderConfig, CopilotModelProviderConfig, CrusoeModelProviderConfig,
    CustomModelProviderConfig, DeepinfraModelProviderConfig, DeepmystModelProviderConfig,
    DeepseekModelProviderConfig, DoubaoModelProviderConfig, FeatherlessModelProviderConfig,
    FireworksModelProviderConfig, FriendliModelProviderConfig, GeminiCliModelProviderConfig,
    GeminiModelProviderConfig, GithubModelsModelProviderConfig, GlmModelProviderConfig,
    GrokCliModelProviderConfig, GroqModelProviderConfig, HailoOllamaModelProviderConfig,
    HuggingfaceModelProviderConfig, HunyuanModelProviderConfig, HyperbolicModelProviderConfig,
    InceptionModelProviderConfig, KiloCliModelProviderConfig, KiloModelProviderConfig,
    LambdaAiModelProviderConfig, LeptonModelProviderConfig, LitellmModelProviderConfig,
    LlamacppModelProviderConfig, LmstudioModelProviderConfig, ManifestModelProviderConfig,
    MinimaxModelProviderConfig, MistralModelProviderConfig, ModelProviderConfig,
    MoonshotModelProviderConfig, MorphModelProviderConfig, NearaiModelProviderConfig,
    NebiusModelProviderConfig, NovitaModelProviderConfig, NscaleModelProviderConfig,
    NvidiaModelProviderConfig, OllamaModelProviderConfig, OpenAIModelProviderConfig,
    OpenRouterModelProviderConfig, OpencodeModelProviderConfig, OsaurusModelProviderConfig,
    OvhModelProviderConfig, PerplexityModelProviderConfig, QianfanModelProviderConfig,
    QwenModelProviderConfig, RekaModelProviderConfig, SambanovaModelProviderConfig,
    SglangModelProviderConfig, SiliconflowModelProviderConfig, StepfunModelProviderConfig,
    SyntheticModelProviderConfig, TelnyxModelProviderConfig, TogetherModelProviderConfig,
    UpstageModelProviderConfig, VeniceModelProviderConfig, VercelModelProviderConfig,
    VllmModelProviderConfig, XaiModelProviderConfig, YiModelProviderConfig, ZaiModelProviderConfig,
    ZerorouterModelProviderConfig,
};
use super::schema::{
    AssemblyAiTranscriptionProviderConfig, DeepgramTranscriptionProviderConfig,
    GoogleTranscriptionProviderConfig, GroqTranscriptionProviderConfig,
    LocalWhisperTranscriptionProviderConfig, OpenAiTranscriptionProviderConfig,
};
use super::schema::{
    EdgeTtsProviderConfig, ElevenLabsTtsProviderConfig, GoogleTtsProviderConfig,
    OpenAITtsProviderConfig, PiperTtsProviderConfig, TtsProviderConfig as TtsBaseConfig,
};
use zeroclaw_api::capability::Capability;

#[macro_export]
macro_rules! define_provider_ref {
    ($name:ident, $category_doc:literal) => {
        #[doc = concat!("Reference to a configured `[", $category_doc, ".<type>.<alias>]` entry.")]
        /// Empty value means "no preference" (opt-out). Non-empty values must
        /// resolve to a configured alias; `Config::validate()` enforces this.
        #[derive(
            Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
        )]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[serde(transparent)]
        pub struct $name(pub String);

        impl $name {
            #[must_use]
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            #[must_use]
            pub fn as_str(&self) -> &str {
                &self.0
            }

            #[must_use]
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }

            #[must_use]
            pub fn into_inner(self) -> String {
                self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                std::fmt::Display::fmt(&self.0, f)
            }
        }

        impl std::ops::Deref for $name {
            type Target = str;
            fn deref(&self) -> &str {
                &self.0
            }
        }

        impl AsRef<str> for $name {
            fn as_ref(&self) -> &str {
                &self.0
            }
        }

        impl From<String> for $name {
            fn from(v: String) -> Self {
                Self(v)
            }
        }

        impl From<&str> for $name {
            fn from(v: &str) -> Self {
                Self(v.to_string())
            }
        }

        impl From<$name> for String {
            fn from(v: $name) -> Self {
                v.0
            }
        }

        impl PartialEq<str> for $name {
            fn eq(&self, other: &str) -> bool {
                self.0 == other
            }
        }

        impl PartialEq<&str> for $name {
            fn eq(&self, other: &&str) -> bool {
                self.0 == *other
            }
        }

        impl PartialEq<String> for $name {
            fn eq(&self, other: &String) -> bool {
                &self.0 == other
            }
        }
    };
}

define_provider_ref!(ModelProviderRef, "providers.models");
define_provider_ref!(TtsProviderRef, "providers.tts");
define_provider_ref!(TranscriptionProviderRef, "providers.transcription");
define_provider_ref!(ChannelRef, "channels");
define_provider_ref!(RiskProfileRef, "risk_profiles");
define_provider_ref!(RuntimeProfileRef, "runtime_profiles");

pub const MAX_FALLBACK_DEPTH: usize = 3;

#[macro_export]
macro_rules! for_each_model_provider_slot {
    ($mac:ident) => {
        $mac! {
            (openai, "openai", OpenAIModelProviderConfig),            (azure, "azure", AzureModelProviderConfig),
            (anthropic, "anthropic", AnthropicModelProviderConfig),            (moonshot, "moonshot", MoonshotModelProviderConfig),
            (qwen, "qwen", QwenModelProviderConfig),
            (glm, "glm", GlmModelProviderConfig),
            (minimax, "minimax", MinimaxModelProviderConfig),
            (zai, "zai", ZaiModelProviderConfig),
            (doubao, "doubao", DoubaoModelProviderConfig),
            (yi, "yi", YiModelProviderConfig),
            (hunyuan, "hunyuan", HunyuanModelProviderConfig),
            (qianfan, "qianfan", QianfanModelProviderConfig),
            (baichuan, "baichuan", BaichuanModelProviderConfig),
            (openrouter, "openrouter", OpenRouterModelProviderConfig),
            (ollama, "ollama", OllamaModelProviderConfig),
            (hailo_ollama, "hailo_ollama", HailoOllamaModelProviderConfig),
            (gemini, "gemini", GeminiModelProviderConfig),
            (gemini_cli, "gemini_cli", GeminiCliModelProviderConfig),
            (grok_cli, "grok_cli", GrokCliModelProviderConfig),
            (bedrock, "bedrock", BedrockModelProviderConfig),
            (telnyx, "telnyx", TelnyxModelProviderConfig),
            (together, "together", TogetherModelProviderConfig),
            (fireworks, "fireworks", FireworksModelProviderConfig),
            (groq, "groq", GroqModelProviderConfig),
            (mistral, "mistral", MistralModelProviderConfig),
            (deepseek, "deepseek", DeepseekModelProviderConfig),
            (atomic_chat, "atomic_chat", AtomicChatModelProviderConfig),
            (cohere, "cohere", CohereModelProviderConfig),
            (perplexity, "perplexity", PerplexityModelProviderConfig),
            (xai, "xai", XaiModelProviderConfig),
            (cerebras, "cerebras", CerebrasModelProviderConfig),
            (crusoe, "crusoe", CrusoeModelProviderConfig),
            (sambanova, "sambanova", SambanovaModelProviderConfig),
            (hyperbolic, "hyperbolic", HyperbolicModelProviderConfig),
            (deepinfra, "deepinfra", DeepinfraModelProviderConfig),
            (huggingface, "huggingface", HuggingfaceModelProviderConfig),
            (ai21, "ai21", Ai21ModelProviderConfig),
            (reka, "reka", RekaModelProviderConfig),
            (baseten, "baseten", BasetenModelProviderConfig),
            (nscale, "nscale", NscaleModelProviderConfig),
            (anyscale, "anyscale", AnyscaleModelProviderConfig),
            (nebius, "nebius", NebiusModelProviderConfig),
            (friendli, "friendli", FriendliModelProviderConfig),
            (stepfun, "stepfun", StepfunModelProviderConfig),
            (aihubmix, "aihubmix", AihubmixModelProviderConfig),
            (siliconflow, "siliconflow", SiliconflowModelProviderConfig),
            (astrai, "astrai", AstraiModelProviderConfig),
            (avian, "avian", AvianModelProviderConfig),
            (deepmyst, "deepmyst", DeepmystModelProviderConfig),
            (venice, "venice", VeniceModelProviderConfig),
            (nearai, "nearai", NearaiModelProviderConfig),
            (novita, "novita", NovitaModelProviderConfig),
            (nvidia, "nvidia", NvidiaModelProviderConfig),
            (vercel, "vercel", VercelModelProviderConfig),
            (cloudflare, "cloudflare", CloudflareModelProviderConfig),
            (atlascloud, "atlascloud", AtlasCloudModelProviderConfig),
            (ovh, "ovh", OvhModelProviderConfig),
            (copilot, "copilot", CopilotModelProviderConfig),
            (lmstudio, "lmstudio", LmstudioModelProviderConfig),
            (llamacpp, "llamacpp", LlamacppModelProviderConfig),
            (sglang, "sglang", SglangModelProviderConfig),
            (vllm, "vllm", VllmModelProviderConfig),
            (osaurus, "osaurus", OsaurusModelProviderConfig),
            (litellm, "litellm", LitellmModelProviderConfig),
            (lepton, "lepton", LeptonModelProviderConfig),
            (manifest, "manifest", ManifestModelProviderConfig),
            (morph, "morph", MorphModelProviderConfig),
            (github_models, "github_models", GithubModelsModelProviderConfig),
            (upstage, "upstage", UpstageModelProviderConfig),
            (featherless, "featherless", FeatherlessModelProviderConfig),
            (arcee, "arcee", ArceeModelProviderConfig),
            (lambda_ai, "lambda_ai", LambdaAiModelProviderConfig),
            (inception, "inception", InceptionModelProviderConfig),
            (synthetic, "synthetic", SyntheticModelProviderConfig),
            (opencode, "opencode", OpencodeModelProviderConfig),
            (kilocli, "kilocli", KiloCliModelProviderConfig),
            (kilo, "kilo", KiloModelProviderConfig),
            (zerorouter, "zerorouter", ZerorouterModelProviderConfig),
            (custom, "custom", CustomModelProviderConfig),
        }
    };
}

macro_rules! emit_model_providers_struct {
    ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
        /// Typed model provider container with one alias map per provider family.
        #[derive(Debug, Clone, Default, Configurable)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[prefix = "providers.models"]
        #[application = "model_provider_refresh"]
        pub struct ModelProviders {
            $(
                #[serde(default, skip_serializing_if = "HashMap::is_empty")]
                #[nested]
                pub $field: HashMap<String, $cfg_ty>,
            )+
            /// Canonical model definitions. This is an in-memory bridge
            /// while the old typed family consumers are migrated. Canonical
            /// and legacy entries may coexist during that migration; each
            /// top-level row keeps its own exact TOML key.
            #[serde(skip)]
            pub(crate) canonical_models: BTreeMap<ModelRef, ModelDefinition<serde_json::Value>>,
        }

        impl serde::Serialize for ModelProviders {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                let mut entries = BTreeMap::<String, serde_json::Value>::new();
                $(
                    if !self.$field.is_empty() {
                        entries.insert(
                            $type_str.to_owned(),
                            serde_json::to_value(&self.$field)
                                .map_err(serde::ser::Error::custom)?,
                        );
                    }
                )+
                for (key, model) in &self.canonical_models {
                    if entries.contains_key(key.as_str()) {
                        return Err(serde::ser::Error::custom(format!(
                            "canonical model `{key}` conflicts with a legacy provider family"
                        )));
                    }
                    entries.insert(
                        key.to_string(),
                        serde_json::to_value(model).map_err(serde::ser::Error::custom)?,
                    );
                }
                entries.serialize(serializer)
            }
        }

        impl<'de> serde::Deserialize<'de> for ModelProviders {
            fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = serde_json::Value::deserialize(deserializer)?;
                let object = raw.as_object().ok_or_else(|| serde::de::Error::custom("providers.models must be a TOML table"))?;
                let mut canonical_models = BTreeMap::new();
                $(let mut $field: HashMap<String, $cfg_ty> = HashMap::new();)+
                for (key, value) in object {
                    if value
                        .as_object()
                        .is_some_and(|entry| entry.contains_key("capability"))
                    {
                        let model = serde_json::from_value(value.clone())
                            .map_err(serde::de::Error::custom)?;
                        canonical_models.insert(ModelRef::from(key.clone()), model);
                        continue;
                    }
                    match key.as_str() {
                        $(
                            $type_str => {
                                $field = serde_json::from_value(value.clone())
                                    .map_err(serde::de::Error::custom)?;
                            }
                        )+
                        _ if key.contains('.') => {
                            return Err(serde::de::Error::custom(format!(
                                "canonical model `{key}` must declare `capability`"
                            )));
                        }
                        _ => {
                            // Unknown legacy families remain parseable so the
                            // startup validator can report them instead of
                            // turning them into malformed canonical models.
                        }
                    }
                }
                Ok(Self {
                    $($field,)+
                    canonical_models,
                })
            }
        }
    };
}
for_each_model_provider_slot!(emit_model_providers_struct);

impl ModelProviders {
    /// Exact canonical model lookup. The key is never split on `.`.
    pub fn canonical_model(
        &self,
        model_ref: &ModelRef,
    ) -> Option<&ModelDefinition<serde_json::Value>> {
        self.canonical_models.get(model_ref)
    }

    /// Insert an exact model key into a caller-owned configuration candidate.
    /// Validate the candidate with `Providers::canonical_registry` before
    /// publishing or persisting it; connection-dependent options resolve there.
    pub fn insert_canonical_model(
        &mut self,
        model_ref: ModelRef,
        model: ModelDefinition<serde_json::Value>,
    ) {
        self.canonical_models.insert(model_ref, model);
    }

    pub fn has_canonical_models(&self) -> bool {
        !self.canonical_models.is_empty()
    }

    /// Iterate canonical model references without exposing the backing map.
    /// Consumers that scope a cloned config must retain exact keys; splitting
    /// these references on `.` would turn a TOML key into a legacy profile.
    pub fn canonical_model_refs(&self) -> impl Iterator<Item = &ModelRef> {
        self.canonical_models.keys()
    }

    /// Retain canonical models by their exact TOML key.
    pub fn retain_canonical_models(
        &mut self,
        mut keep: impl FnMut(&ModelRef, &ModelDefinition<serde_json::Value>) -> bool,
    ) {
        self.canonical_models
            .retain(|model_ref, model| keep(model_ref, model));
    }

    /// Whether the canonical exact-key map contains at least one model for a
    /// capability. Consumers use this to decide whether an enabled subsystem
    /// can rely on canonical registration before falling back to legacy maps.
    pub fn has_canonical_capability(&self, capability: Capability) -> bool {
        self.canonical_models
            .values()
            .any(|model| model.capability() == capability)
    }

    /// Iterate every entry across every typed slot in a deterministic
    /// order: slot order (as declared by `for_each_model_provider_slot!`)
    /// first, then alias order within each slot. The alias maps are
    /// `HashMap`s whose iteration order is seeded per process, so the
    /// sort is what keeps "first entry" consumers — the gateway boot
    /// default provider/model, first-run agent defaults, the
    /// ACP-advertised default model — stable across restarts.
    pub fn iter_entries(&self) -> impl Iterator<Item = (&'static str, &str, &ModelProviderConfig)> {
        let mut out: Vec<(&'static str, &str, &ModelProviderConfig)> = Vec::new();
        macro_rules! emit_iter {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $(
                    let mut slot: Vec<(&String, &$cfg_ty)> = self.$field.iter().collect();
                    slot.sort_by(|a, b| a.0.cmp(b.0));
                    out.extend(
                        slot.into_iter()
                            .map(|(alias, cfg)| ($type_str, alias.as_str(), &cfg.base)),
                    );
                )+
            };
        }
        for_each_model_provider_slot!(emit_iter);
        out.into_iter()
    }

    /// Iterate every entry mutably across every typed slot, in the same
    /// deterministic order as [`ModelProviders::iter_entries`].
    pub fn iter_entries_mut(
        &mut self,
    ) -> impl Iterator<Item = (&'static str, &str, &mut ModelProviderConfig)> {
        let mut out: Vec<(&'static str, &str, &mut ModelProviderConfig)> = Vec::new();
        macro_rules! emit_iter_mut {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $(
                    let mut slot: Vec<(&String, &mut $cfg_ty)> = self.$field.iter_mut().collect();
                    slot.sort_by(|a, b| a.0.cmp(b.0));
                    out.extend(
                        slot.into_iter()
                            .map(|(alias, cfg)| ($type_str, alias.as_str(), &mut cfg.base)),
                    );
                )+
            };
        }
        for_each_model_provider_slot!(emit_iter_mut);
        out.into_iter()
    }

    /// First entry across every typed slot that declares a non-empty `model`,
    /// in the existing `iter_entries` order. An entry without a `model`
    /// cannot serve as an install-wide default — there is no model string to
    /// pair with the provider — so entries without one are skipped instead of
    /// seeding a provider/model mismatch at the consumer.
    #[must_use]
    pub fn first_entry_with_model(&self) -> Option<(&'static str, &str, &ModelProviderConfig)> {
        self.iter_entries().find(|(_, _, base)| {
            base.model
                .as_deref()
                .map(str::trim)
                .is_some_and(|model| !model.is_empty())
        })
    }

    /// Resolve the family-default endpoint URI for `<family>.<alias>`. Returns
    /// `None` when the family is single-endpoint, unknown, or the alias is
    /// missing. Dispatch is generated by `for_each_model_provider_slot!`, so
    /// adding a family without a `FamilyEndpoint` impl is a compile error.
    pub fn resolved_endpoint_uri(&self, family: &str, alias: &str) -> Option<&'static str> {
        use super::schema::FamilyEndpoint;
        macro_rules! emit_endpoint {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $( $type_str => self.$field.get(alias).and_then(|cfg| cfg.endpoint_uri()), )+
                    _ => None,
                }
            };
        }
        for_each_model_provider_slot!(emit_endpoint)
    }

    /// Look up the shared base config for a given `<provider_type>.<alias>`
    /// pair. Returns `None` when the family isn't recognized OR when
    /// the alias doesn't exist in that family's typed slot.
    pub fn find(&self, family: &str, alias: &str) -> Option<&ModelProviderConfig> {
        macro_rules! emit_get {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $( $type_str => self.$field.get(alias).map(|cfg| &cfg.base), )+
                    _ => None,
                }
            };
        }
        for_each_model_provider_slot!(emit_get)
    }

    /// Look up a model entry `<family>.<alias>.models.<model_alias>`. Returns
    /// `None` when the provider profile is missing or has no such model entry.
    #[must_use]
    pub fn find_model(
        &self,
        family: &str,
        alias: &str,
        model_alias: &str,
    ) -> Option<&super::schema::ModelEntryConfig> {
        self.find(family, alias)?.models.get(model_alias)
    }

    /// Resolve the longest configured profile alias, including aliases with
    /// dots. Configured profiles own the boundary between profile and model.
    pub fn profile_ref<'a>(&'a self, name: &'a str) -> Option<(&'a str, &'a str)> {
        let (family, tail) = name.trim().split_once('.')?;
        self.iter_entries()
            .filter(|(ty, alias, _)| {
                *ty == family
                    && (tail == *alias
                        || tail
                            .strip_prefix(*alias)
                            .is_some_and(|suffix| suffix.starts_with('.') && suffix.len() > 1))
            })
            .max_by_key(|(_, alias, _)| alias.len())
            .map(|(ty, alias, _)| (ty, alias))
            .or_else(|| super::schema::provider_profile_ref(name))
    }

    pub fn find_by_name(&self, name: &str) -> Option<(&'static str, String, &ModelProviderConfig)> {
        if let Some((kind, alias)) = self.profile_ref(name) {
            return self
                .iter_entries()
                .find(|(ty, key, _)| *ty == kind && *key == alias)
                .map(|(ty, key, entry)| (ty, key.to_string(), entry));
        }
        let mut hit: Option<(&'static str, String, &ModelProviderConfig)> = None;
        macro_rules! emit_bare {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $(
                    if let Some(c) = self.$field.get(name) {
                        if hit.is_some() {
                            return None; // ambiguous across kinds
                        }
                        hit = Some(($type_str, name.to_string(), &c.base));
                    }
                )+
            };
        }
        for_each_model_provider_slot!(emit_bare);
        hit
    }

    pub fn ensure(&mut self, family: &str, alias: &str) -> Option<&mut ModelProviderConfig> {
        macro_rules! emit_ensure {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $(
                        $type_str => Some(
                            &mut self
                                .$field
                                .entry(alias.to_string())
                                .or_default()
                                .base,
                        ),
                    )+
                    _ => None,
                }
            };
        }
        for_each_model_provider_slot!(emit_ensure)
    }

    /// True when `family`'s typed slot has at least one configured
    /// alias entry. Returns `false` for unknown families.
    pub fn contains_model_provider_type(&self, family: &str) -> bool {
        macro_rules! emit_contains {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $( $type_str => !self.$field.is_empty(), )+
                    _ => false,
                }
            };
        }
        for_each_model_provider_slot!(emit_contains)
    }

    /// Iterate the alias keys for a given model_provider type. Returns an empty
    /// iterator for unknown model_provider types.
    pub fn aliases_of<'a>(&'a self, family: &str) -> Box<dyn Iterator<Item = &'a str> + 'a> {
        macro_rules! emit_aliases {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $( $type_str => Box::new(self.$field.keys().map(String::as_str)), )+
                    _ => Box::new(std::iter::empty()),
                }
            };
        }
        for_each_model_provider_slot!(emit_aliases)
    }

    /// Canonical family slot names, straight from
    /// `for_each_model_provider_slot!`. Use this to distinguish "unknown
    /// family" from "known family, missing alias" in validation messages,
    /// and to detect raw-TOML sections that deserialization silently drops.
    #[must_use]
    pub fn slot_names() -> &'static [&'static str] {
        macro_rules! emit_slot_names {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                &[$($type_str),+]
            };
        }
        const NAMES: &[&str] = for_each_model_provider_slot!(emit_slot_names);
        NAMES
    }

    /// Remove the entry for `<provider_type>.<alias>`, returning whether it
    /// existed. Returns `false` for unknown families.
    pub fn remove_alias(&mut self, family: &str, alias: &str) -> bool {
        macro_rules! emit_remove {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                match family {
                    $( $type_str => self.$field.remove(alias).is_some(), )+
                    _ => false,
                }
            };
        }
        for_each_model_provider_slot!(emit_remove)
    }

    /// True when no slot has any entry.
    pub fn is_empty(&self) -> bool {
        macro_rules! emit_is_empty {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $( self.$field.is_empty() && )+ true
            };
        }
        for_each_model_provider_slot!(emit_is_empty)
    }

    /// Total number of (provider_type, alias) entries across all slots.
    pub fn len(&self) -> usize {
        macro_rules! emit_len {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                0 $( + self.$field.len() )+
            };
        }
        for_each_model_provider_slot!(emit_len)
    }
}

/// Typed TTS-provider container — one slot per TTS family. Mirrors
/// `ModelProviders` but smaller (TTS has a closed set of 5 families:
/// openai, elevenlabs, google, edge, piper). No catch-all needed.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers.tts"]
pub struct TtsProviders {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub openai: HashMap<String, OpenAITtsProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub elevenlabs: HashMap<String, ElevenLabsTtsProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub google: HashMap<String, GoogleTtsProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub edge: HashMap<String, EdgeTtsProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub piper: HashMap<String, PiperTtsProviderConfig>,
}

impl TtsProviders {
    /// Iterate every TTS entry across every typed slot, yielding
    /// `(family, alias, &base)` triples.
    pub fn iter_entries(
        &self,
    ) -> Box<dyn Iterator<Item = (&'static str, &str, &TtsBaseConfig)> + '_> {
        Box::new(
            std::iter::empty()
                .chain(
                    self.openai
                        .iter()
                        .map(|(a, c)| ("openai", a.as_str(), &c.base)),
                )
                .chain(
                    self.elevenlabs
                        .iter()
                        .map(|(a, c)| ("elevenlabs", a.as_str(), &c.base)),
                )
                .chain(
                    self.google
                        .iter()
                        .map(|(a, c)| ("google", a.as_str(), &c.base)),
                )
                .chain(self.edge.iter().map(|(a, c)| ("edge", a.as_str(), &c.base)))
                .chain(
                    self.piper
                        .iter()
                        .map(|(a, c)| ("piper", a.as_str(), &c.base)),
                ),
        )
    }

    /// Iterate every TTS entry mutably across every typed slot.
    pub fn iter_entries_mut(
        &mut self,
    ) -> Box<dyn Iterator<Item = (&'static str, &str, &mut TtsBaseConfig)> + '_> {
        Box::new(
            std::iter::empty()
                .chain(
                    self.openai
                        .iter_mut()
                        .map(|(a, c)| ("openai", a.as_str(), &mut c.base)),
                )
                .chain(
                    self.elevenlabs
                        .iter_mut()
                        .map(|(a, c)| ("elevenlabs", a.as_str(), &mut c.base)),
                )
                .chain(
                    self.google
                        .iter_mut()
                        .map(|(a, c)| ("google", a.as_str(), &mut c.base)),
                )
                .chain(
                    self.edge
                        .iter_mut()
                        .map(|(a, c)| ("edge", a.as_str(), &mut c.base)),
                )
                .chain(
                    self.piper
                        .iter_mut()
                        .map(|(a, c)| ("piper", a.as_str(), &mut c.base)),
                ),
        )
    }

    /// True when no slot has any entry.
    pub fn is_empty(&self) -> bool {
        self.openai.is_empty()
            && self.elevenlabs.is_empty()
            && self.google.is_empty()
            && self.edge.is_empty()
            && self.piper.is_empty()
    }
}

/// Typed transcription-provider container — one slot per STT family.
/// Mirrors `ModelProviders` / `TtsProviders`. Closed set of 6 families:
/// groq, openai, deepgram, assemblyai, google, local_whisper.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers.transcription"]
pub struct TranscriptionProviders {
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub groq: HashMap<String, GroqTranscriptionProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub openai: HashMap<String, OpenAiTranscriptionProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub deepgram: HashMap<String, DeepgramTranscriptionProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub assemblyai: HashMap<String, AssemblyAiTranscriptionProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub google: HashMap<String, GoogleTranscriptionProviderConfig>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    #[nested]
    pub local_whisper: HashMap<String, LocalWhisperTranscriptionProviderConfig>,
}

pub enum TranscriptionProviderEntry<'a> {
    Groq(&'a GroqTranscriptionProviderConfig),
    OpenAi(&'a OpenAiTranscriptionProviderConfig),
    Deepgram(&'a DeepgramTranscriptionProviderConfig),
    AssemblyAi(&'a AssemblyAiTranscriptionProviderConfig),
    Google(&'a GoogleTranscriptionProviderConfig),
    LocalWhisper(&'a LocalWhisperTranscriptionProviderConfig),
}

impl TranscriptionProviders {
    /// True when no slot has any entry.
    pub fn is_empty(&self) -> bool {
        self.groq.is_empty()
            && self.openai.is_empty()
            && self.deepgram.is_empty()
            && self.assemblyai.is_empty()
            && self.google.is_empty()
            && self.local_whisper.is_empty()
    }

    /// Iterate every configured (family, alias) pair across all six slots.
    pub fn iter_aliases(&self) -> impl Iterator<Item = (&'static str, &str)> {
        let mut out: Vec<(&'static str, &str)> = Vec::new();
        for k in self.groq.keys() {
            out.push(("groq", k.as_str()));
        }
        for k in self.openai.keys() {
            out.push(("openai", k.as_str()));
        }
        for k in self.deepgram.keys() {
            out.push(("deepgram", k.as_str()));
        }
        for k in self.assemblyai.keys() {
            out.push(("assemblyai", k.as_str()));
        }
        for k in self.google.keys() {
            out.push(("google", k.as_str()));
        }
        for k in self.local_whisper.keys() {
            out.push(("local_whisper", k.as_str()));
        }
        out.into_iter()
    }

    pub fn iter_entries(
        &self,
    ) -> Box<dyn Iterator<Item = (&'static str, &str, TranscriptionProviderEntry<'_>)> + '_> {
        Box::new(
            std::iter::empty()
                .chain(self.groq.iter().map(|(alias, config)| {
                    (
                        "groq",
                        alias.as_str(),
                        TranscriptionProviderEntry::Groq(config),
                    )
                }))
                .chain(self.openai.iter().map(|(alias, config)| {
                    (
                        "openai",
                        alias.as_str(),
                        TranscriptionProviderEntry::OpenAi(config),
                    )
                }))
                .chain(self.deepgram.iter().map(|(alias, config)| {
                    (
                        "deepgram",
                        alias.as_str(),
                        TranscriptionProviderEntry::Deepgram(config),
                    )
                }))
                .chain(self.assemblyai.iter().map(|(alias, config)| {
                    (
                        "assemblyai",
                        alias.as_str(),
                        TranscriptionProviderEntry::AssemblyAi(config),
                    )
                }))
                .chain(self.google.iter().map(|(alias, config)| {
                    (
                        "google",
                        alias.as_str(),
                        TranscriptionProviderEntry::Google(config),
                    )
                }))
                .chain(self.local_whisper.iter().map(|(alias, config)| {
                    (
                        "local_whisper",
                        alias.as_str(),
                        TranscriptionProviderEntry::LocalWhisper(config),
                    )
                })),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_iter_entries_sorts_aliases_within_each_slot() {
        let mut providers = ModelProviders::default();
        // Insert in non-sorted order: the alias maps are HashMaps whose
        // iteration order is seeded per process, so only iter_entries' own
        // sort can give first-entry consumers a stable pick across restarts.
        for alias in ["zeta", "alpha", "mid"] {
            providers
                .custom
                .insert(alias.to_string(), CustomModelProviderConfig::default());
        }

        let entries: Vec<(&str, &str)> = providers
            .iter_entries()
            .map(|(family, alias, _)| (family, alias))
            .collect();
        assert_eq!(
            entries,
            vec![("custom", "alpha"), ("custom", "mid"), ("custom", "zeta")]
        );

        // The mutable walk yields the same deterministic order. Collect
        // owned copies so the immutable borrow above can end before
        // iter_entries_mut takes the mutable one.
        let mut_entries: Vec<(String, String)> = providers
            .iter_entries_mut()
            .map(|(family, alias, _)| (family.to_string(), alias.to_string()))
            .collect();
        assert_eq!(
            mut_entries,
            vec![
                ("custom".into(), "alpha".into()),
                ("custom".into(), "mid".into()),
                ("custom".into(), "zeta".into()),
            ]
        );
    }

    #[test]
    fn first_entry_with_model_skips_model_less_entries() {
        let mut providers = ModelProviders::default();
        providers
            .openai
            .insert("alpha".to_string(), OpenAIModelProviderConfig::default());
        providers.openai.insert(
            "beta".to_string(),
            OpenAIModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("beta-model".to_string()),
                    ..Default::default()
                },
            },
        );
        providers.ollama.insert(
            "gamma".to_string(),
            OllamaModelProviderConfig {
                base: ModelProviderConfig {
                    model: Some("gamma-model".to_string()),
                    ..Default::default()
                },
                ..OllamaModelProviderConfig::default()
            },
        );

        // Openai precedes ollama in slot order. Within openai, only "beta"
        // has a model, so it is selected regardless of alias iteration order.
        let first = providers
            .first_entry_with_model()
            .expect("an entry with a model must be found");
        assert_eq!(
            (first.0, first.1, first.2.model.as_deref()),
            ("openai", "beta", Some("beta-model"))
        );

        // A whitespace-only model counts as unset, so the pick moves on to
        // the next entry with a real model.
        providers.openai.get_mut("beta").unwrap().base.model = Some("   ".to_string());
        let first = providers
            .first_entry_with_model()
            .expect("a later entry with a model must be found");
        assert_eq!((first.0, first.1), ("ollama", "gamma"));
    }

    #[test]
    fn transcription_iter_entries_walks_every_typed_slot() {
        let mut providers = TranscriptionProviders::default();
        providers
            .groq
            .insert("fast".into(), GroqTranscriptionProviderConfig::default());
        providers.openai.insert(
            "whisper".into(),
            OpenAiTranscriptionProviderConfig::default(),
        );
        providers.deepgram.insert(
            "nova".into(),
            DeepgramTranscriptionProviderConfig::default(),
        );
        providers.assemblyai.insert(
            "default".into(),
            AssemblyAiTranscriptionProviderConfig::default(),
        );
        providers.google.insert(
            "speech".into(),
            GoogleTranscriptionProviderConfig::default(),
        );
        providers.local_whisper.insert(
            "lan".into(),
            LocalWhisperTranscriptionProviderConfig::default(),
        );

        let entries: Vec<(&str, String, &str)> = providers
            .iter_entries()
            .map(|(family, alias, entry)| {
                let variant = match entry {
                    TranscriptionProviderEntry::Groq(_) => "groq",
                    TranscriptionProviderEntry::OpenAi(_) => "openai",
                    TranscriptionProviderEntry::Deepgram(_) => "deepgram",
                    TranscriptionProviderEntry::AssemblyAi(_) => "assemblyai",
                    TranscriptionProviderEntry::Google(_) => "google",
                    TranscriptionProviderEntry::LocalWhisper(_) => "local_whisper",
                };
                (family, alias.to_string(), variant)
            })
            .collect();

        assert_eq!(
            entries,
            vec![
                ("groq", "fast".into(), "groq"),
                ("openai", "whisper".into(), "openai"),
                ("deepgram", "nova".into(), "deepgram"),
                ("assemblyai", "default".into(), "assemblyai"),
                ("google", "speech".into(), "google"),
                ("local_whisper", "lan".into(), "local_whisper"),
            ]
        );
    }
}

/// Top-level wrapper for every configured provider category.
#[derive(Debug, Clone, Default, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers"]
#[secret_hook = "crate::providers::canonical_connection_secrets"]
pub struct Providers {
    /// Model providers — `[providers.models.<type>.<alias>]`.
    #[serde(default)]
    #[nested]
    pub models: ModelProviders,

    /// Text-to-speech providers — `[providers.tts.<type>.<alias>]`.
    #[serde(default)]
    #[nested]
    pub tts: TtsProviders,

    /// Transcription / speech-to-text providers — `[providers.transcription.<type>.<alias>]`.
    #[serde(default)]
    #[nested]
    pub transcription: TranscriptionProviders,

    /// Canonical connections. They are parsed at the provider root and are
    /// intentionally excluded from the legacy configurable property surface;
    /// edits use the registry boundary instead of synthesizing dotted paths.
    #[serde(skip)]
    pub connections: std::collections::BTreeMap<ProviderId, ProviderConnection>,
    #[serde(skip)]
    pub model_presets: std::collections::BTreeMap<super::provider_registry::PresetId, ModelPreset>,
    #[serde(skip)]
    pub fallback: std::collections::BTreeMap<ModelRef, Vec<ModelRef>>,
}

/// Secret traversal for canonical connection authentication. Connections are
/// intentionally outside the configurable property surface, but their auth
/// values still follow the same load/save encryption lifecycle as legacy
/// provider entries.
pub(crate) mod canonical_connection_secrets {
    use super::*;

    pub fn secret_fields(providers: &Providers, fields: &mut Vec<SecretFieldInfo>) {
        fields.push(SecretFieldInfo {
            name: "providers.connections",
            category: "providers",
            is_set: providers
                .connections
                .values()
                .any(|connection| connection.has_secrets()),
        });
    }

    pub fn encrypt_secrets(providers: &mut Providers, store: &SecretStore) -> anyhow::Result<()> {
        for (provider_id, connection) in &mut providers.connections {
            connection.encrypt_secrets(store, &format!("providers.connections.{provider_id}"))?;
        }
        Ok(())
    }

    pub fn decrypt_secrets(providers: &mut Providers, store: &SecretStore) -> anyhow::Result<()> {
        for (provider_id, connection) in &mut providers.connections {
            connection.decrypt_secrets(store, &format!("providers.connections.{provider_id}"))?;
        }
        Ok(())
    }

    pub fn mask_secrets(providers: &mut Providers) {
        for connection in providers.connections.values_mut() {
            connection.mask_secrets();
        }
    }

    pub fn restore_secrets_from(providers: &mut Providers, current: &Providers) {
        for (provider_id, connection) in &mut providers.connections {
            if let Some(current_connection) = current.connections.get(provider_id) {
                connection.restore_secrets_from(current_connection);
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct ProvidersWire {
    models: ModelProviders,
    tts: TtsProviders,
    transcription: TranscriptionProviders,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    connections: std::collections::BTreeMap<ProviderId, ProviderConnection>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    model_presets: std::collections::BTreeMap<super::provider_registry::PresetId, ModelPreset>,
    #[serde(skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    fallback: std::collections::BTreeMap<ModelRef, Vec<ModelRef>>,
}

impl Serialize for Providers {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        ProvidersWire {
            models: self.models.clone(),
            tts: self.tts.clone(),
            transcription: self.transcription.clone(),
            connections: self.connections.clone(),
            model_presets: self.model_presets.clone(),
            fallback: self.fallback.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Providers {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = ProvidersWire::deserialize(deserializer)?;
        let mut providers = Self {
            models: wire.models,
            tts: wire.tts,
            transcription: wire.transcription,
            connections: wire.connections,
            model_presets: wire.model_presets,
            fallback: wire.fallback,
        };
        if !providers.models.canonical_models.is_empty()
            || !providers.connections.is_empty()
            || !providers.model_presets.is_empty()
            || !providers.fallback.is_empty()
        {
            let raw_models = std::mem::take(&mut providers.models.canonical_models);
            let registry = ProviderRegistry::from_raw_parts(
                providers.connections.clone(),
                raw_models.clone(),
                providers.model_presets.clone(),
                providers.fallback.clone(),
            )
            .map_err(serde::de::Error::custom)?;
            providers.connections = registry.connections;
            providers.model_presets = registry.model_presets;
            providers.fallback = registry.fallback;
            providers.models.canonical_models = raw_models;
        }
        Ok(providers)
    }
}

impl Providers {
    /// Build the validated canonical registry for a consumer admission. This
    /// view is materialized on demand and is never stored as a second config
    /// snapshot.
    pub fn canonical_registry(
        &self,
    ) -> Result<ProviderRegistry, super::provider_registry::RegistryError> {
        // Compatibility is confined to this config read boundary. Consumers
        // receive one exact-key registry and never parse family/profile refs.
        let mut connections = self.connections.clone();
        let mut models = self.models.canonical_models.clone();
        let legacy = serde_json::to_value(&self.models).map_err(|error| {
            super::provider_registry::RegistryError::new(["providers", "models"], error.to_string())
        })?;
        for (family, alias, profile) in self.models.iter_entries() {
            let profile_ref = format!("{family}.{alias}");
            let mut raw = legacy[family][alias].clone();
            if profile.uri.is_none()
                && family != "qwen"
                && let Some(uri) = self.models.resolved_endpoint_uri(family, alias)
            {
                raw["uri"] = serde_json::Value::String(uri.to_string());
            }
            let selected = profile.models.get("default").or_else(|| {
                (profile.models.len() == 1)
                    .then(|| profile.models.values().next())
                    .flatten()
            });
            let entries = std::iter::once((profile_ref.clone(), selected)).chain(
                profile
                    .models
                    .iter()
                    .map(|(name, entry)| (format!("{profile_ref}.{name}"), Some(entry))),
            );
            for (reference, entry) in entries {
                let model_ref = ModelRef::from(reference.as_str());
                if models.contains_key(&model_ref) {
                    continue;
                }
                let (connection, mut model) = crate::migration::legacy_chat_definition(
                    &raw, family, entry, None,
                )
                .map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "models", reference.as_str()],
                        error.to_string(),
                    )
                })?;
                let provider_id = ProviderId::from(format!("legacy.{profile_ref}"));
                if self.connections.contains_key(&provider_id) {
                    return Err(super::provider_registry::RegistryError::new(
                        ["providers", "connections", provider_id.as_str()],
                        "legacy connection name conflicts with an explicit connection",
                    ));
                }
                let connection = serde_json::from_value(connection).map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "models", reference.as_str()],
                        error.to_string(),
                    )
                })?;
                connections.entry(provider_id.clone()).or_insert(connection);
                model["provider"] = serde_json::Value::String(provider_id.to_string());
                models.insert(
                    model_ref,
                    serde_json::from_value(model).map_err(|error| {
                        super::provider_registry::RegistryError::new(
                            ["providers", "models", reference.as_str()],
                            error.to_string(),
                        )
                    })?,
                );
            }
        }

        // Project the remaining typed compatibility tables into the same
        // exact-key registry. This keeps TTS/ASR consumers independent from
        // their retired family maps while preserving their legacy defaults.
        for (family, alias, base) in self.tts.iter_entries() {
            let legacy_ref = ModelRef::from(format!("{family}.{alias}"));
            let reference = if models.contains_key(&legacy_ref) {
                ModelRef::from(format!("tts.{family}.{alias}"))
            } else {
                legacy_ref
            };
            if models.contains_key(&reference) {
                continue;
            }
            let provider_id = ProviderId::from(format!("legacy.tts.{family}.{alias}"));
            let mut connection = serde_json::json!({"kind": family});
            if let Some(uri) = base.uri.as_deref() {
                connection["uri"] = serde_json::Value::String(uri.to_owned());
            }
            if let Some(api_key) = base.api_key.as_deref() {
                connection["auth"] = serde_json::json!({"mode":"api_key","api_key":api_key});
            }
            let mut options = serde_json::Map::new();
            for (field, value) in [
                ("voice", base.voice.clone().map(serde_json::Value::String)),
                ("speed", base.speed.map(serde_json::Value::from)),
                ("stability", base.stability.map(serde_json::Value::from)),
                (
                    "similarity_boost",
                    base.similarity_boost.map(serde_json::Value::from),
                ),
                (
                    "language",
                    base.language_code.clone().map(serde_json::Value::String),
                ),
                (
                    "format",
                    base.response_format.clone().map(serde_json::Value::String),
                ),
            ] {
                if let Some(value) = value {
                    options.insert(field.to_owned(), value);
                }
            }
            let mut model = serde_json::json!({
                "capability": "tts",
                "provider": provider_id.as_str(),
                "options": options,
            });
            if let Some(model_id) = base.model.as_deref() {
                model["model"] = serde_json::Value::String(model_id.to_owned());
            }
            connections.insert(
                provider_id.clone(),
                serde_json::from_value(connection).map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "tts", family, alias],
                        error.to_string(),
                    )
                })?,
            );
            models.insert(
                reference,
                serde_json::from_value(model).map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "tts", family, alias],
                        error.to_string(),
                    )
                })?,
            );
        }
        for (family, alias, entry) in self.transcription.iter_entries() {
            let legacy_ref = ModelRef::from(format!("{family}.{alias}"));
            let reference = if models.contains_key(&legacy_ref) {
                ModelRef::from(format!("asr.{family}.{alias}"))
            } else {
                legacy_ref
            };
            if models.contains_key(&reference) {
                continue;
            }
            let raw = match entry {
                TranscriptionProviderEntry::Groq(value) => serde_json::to_value(value),
                TranscriptionProviderEntry::OpenAi(value) => serde_json::to_value(value),
                TranscriptionProviderEntry::Deepgram(value) => serde_json::to_value(value),
                TranscriptionProviderEntry::AssemblyAi(value) => serde_json::to_value(value),
                TranscriptionProviderEntry::Google(value) => serde_json::to_value(value),
                TranscriptionProviderEntry::LocalWhisper(value) => serde_json::to_value(value),
            }
            .map_err(|error| {
                super::provider_registry::RegistryError::new(
                    ["providers", "transcription", family, alias],
                    error.to_string(),
                )
            })?;
            let provider_id = ProviderId::from(format!("legacy.asr.{family}.{alias}"));
            let kind = if family == "openai" { "openai" } else { family };
            let mut connection = serde_json::json!({"kind": kind});
            if let Some(uri) = raw.get("uri").and_then(serde_json::Value::as_str) {
                connection["uri"] = serde_json::Value::String(uri.to_owned());
            }
            if let Some(api_key) = raw.get("api_key").and_then(serde_json::Value::as_str) {
                connection["auth"] = serde_json::json!({"mode":"api_key","api_key":api_key});
            }
            let model_id = raw
                .get("model")
                .cloned()
                .unwrap_or_else(|| serde_json::Value::String("whisper-1".into()));
            let model = serde_json::json!({"capability":"asr","provider":provider_id.as_str(),"model":model_id});
            connections.insert(
                provider_id.clone(),
                serde_json::from_value(connection).map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "transcription", family, alias],
                        error.to_string(),
                    )
                })?,
            );
            models.insert(
                reference,
                serde_json::from_value(model).map_err(|error| {
                    super::provider_registry::RegistryError::new(
                        ["providers", "transcription", family, alias],
                        error.to_string(),
                    )
                })?,
            );
        }
        ProviderRegistry::from_raw_parts(
            connections,
            models,
            self.model_presets.clone(),
            self.fallback.clone(),
        )
    }
}

macro_rules! emit_model_cost_rates_struct {
    ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
        /// `[cost.rates.providers.models.<type>.<model>]` — token-cost rates
        /// per (provider type, model). One slot per provider type; each
        /// slot is a `HashMap<model_id, ModelCostRates>`. The slot list
        /// matches `ModelProviders` byte-for-byte (same source macro).
        #[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[prefix = "cost.rates.providers.models"]
        pub struct ModelCostRatesByProvider {
            $(
                #[serde(default, skip_serializing_if = "HashMap::is_empty")]
                #[nested]
                #[resource_key]
                pub $field: HashMap<String, super::schema::ModelCostRates>,
            )+
        }

        impl ModelCostRatesByProvider {
            /// Lookup rates by `(provider_type, model_id)`.
            #[must_use]
            pub fn get(
                &self,
                provider_type: &str,
                model_id: &str,
            ) -> Option<&super::schema::ModelCostRates> {
                match provider_type {
                    $(
                        $type_str => self.$field.get(model_id),
                    )+
                    _ => None,
                }
            }

            /// Iterate every priced model across every slot, yielding
            /// `(provider_type, model_id, &rates)` triples. Mirrors
            /// `ModelProviders::iter_entries` so callers can walk both
            /// the providers and the rate sheet with the same loop shape.
            pub fn iter_entries(
                &self,
            ) -> impl Iterator<Item = (&'static str, &str, &super::schema::ModelCostRates)> {
                let mut out: Vec<(&'static str, &str, &super::schema::ModelCostRates)> = Vec::new();
                $(
                    for (model_id, rates) in &self.$field {
                        out.push(($type_str, model_id.as_str(), rates));
                    }
                )+
                out.into_iter()
            }

            /// True when no slot has any priced model.
            pub fn is_empty(&self) -> bool {
                $(self.$field.is_empty())&&+
            }
        }
    };
}
for_each_model_provider_slot!(emit_model_cost_rates_struct);

/// Slot list for TTS providers. Single source of truth shared between
/// the typed-provider wrapper and the cost-rates wrapper — adding a TTS
/// family is a one-line edit here.
#[macro_export]
macro_rules! for_each_tts_provider_slot {
    ($mac:ident, $rate_ty:ty) => {
        $mac! {
            $rate_ty,
            (openai, "openai"),
            (elevenlabs, "elevenlabs"),
            (google, "google"),
            (edge, "edge"),
            (piper, "piper"),
        }
    };
}

/// Slot list for transcription providers.
#[macro_export]
macro_rules! for_each_transcription_provider_slot {
    ($mac:ident, $rate_ty:ty) => {
        $mac! {
            $rate_ty,
            (groq, "groq"),
            (openai, "openai"),
            (deepgram, "deepgram"),
            (assemblyai, "assemblyai"),
            (google, "google"),
            (local_whisper, "local_whisper"),
        }
    };
}

/// Collect the `$type_str` names out of a rate-typed slot macro
/// (`for_each_tts_provider_slot!` / `for_each_transcription_provider_slot!`).
/// The `$rate_ty` head is consumed and ignored; the macro exists so
/// `slot_names()` derives from the same source the structs are built from.
macro_rules! collect_rate_slot_names {
    ($rate_ty:ty, $(($field:ident, $type_str:literal)),+ $(,)?) => {
        &[$($type_str),+]
    };
}

impl TtsProviders {
    /// Canonical TTS family slot names, derived from
    /// `for_each_tts_provider_slot!`.
    #[must_use]
    pub fn slot_names() -> &'static [&'static str] {
        const NAMES: &[&str] = for_each_tts_provider_slot!(collect_rate_slot_names, ());
        NAMES
    }
}

impl TranscriptionProviders {
    /// Canonical transcription family slot names, derived from
    /// `for_each_transcription_provider_slot!`.
    #[must_use]
    pub fn slot_names() -> &'static [&'static str] {
        const NAMES: &[&str] = for_each_transcription_provider_slot!(collect_rate_slot_names, ());
        NAMES
    }
}

/// Emit a `<Family>CostRatesByProvider` struct from a slot list. Used
/// by both the TTS and transcription cost-rate wrappers — every field,
/// every dispatch arm, every iter row expands from one slot list. No
/// hand-typed `match "openai" => self.openai` tables anywhere.
macro_rules! emit_simple_cost_rates_struct {
    (
        $struct_name:ident,
        $rate_ty:ty,
        $prefix:literal,
        $resource_doc:literal,
        $(($field:ident, $type_str:literal)),+ $(,)?
    ) => {
        #[doc = concat!("`", $prefix, ".<type>.<", $resource_doc, ">`")]
        ///  — per-(provider type, resource) cost rates.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[prefix = $prefix]
        pub struct $struct_name {
            $(
                #[serde(default, skip_serializing_if = "HashMap::is_empty")]
                #[nested]
                #[resource_key]
                pub $field: HashMap<String, $rate_ty>,
            )+
        }

        impl $struct_name {
            /// Lookup rates by `(provider_type, resource_id)`.
            #[must_use]
            pub fn get(&self, provider_type: &str, resource_id: &str) -> Option<&$rate_ty> {
                match provider_type {
                    $($type_str => self.$field.get(resource_id),)+
                    _ => None,
                }
            }

            /// Iterate `(provider_type, resource_id, &rates)` across every
            /// slot.
            pub fn iter_entries(
                &self,
            ) -> impl Iterator<Item = (&'static str, &str, &$rate_ty)> {
                let mut out: Vec<(&'static str, &str, &$rate_ty)> = Vec::new();
                $(
                    for (resource_id, rates) in &self.$field {
                        out.push(($type_str, resource_id.as_str(), rates));
                    }
                )+
                out.into_iter()
            }

            /// True when no slot has any priced resource.
            pub fn is_empty(&self) -> bool {
                $(self.$field.is_empty())&&+
            }
        }
    };
}

macro_rules! emit_tts_cost_rates_struct {
    ($rate_ty:ty, $($slot:tt),+ $(,)?) => {
        emit_simple_cost_rates_struct! {
            TtsCostRatesByProvider,
            $rate_ty,
            "cost.rates.providers.tts",
            "voice",
            $($slot),+
        }
    };
}
for_each_tts_provider_slot!(emit_tts_cost_rates_struct, super::schema::TtsCostRates);

macro_rules! emit_transcription_cost_rates_struct {
    ($rate_ty:ty, $($slot:tt),+ $(,)?) => {
        emit_simple_cost_rates_struct! {
            TranscriptionCostRatesByProvider,
            $rate_ty,
            "cost.rates.providers.transcription",
            "model",
            $($slot),+
        }
    };
}
for_each_transcription_provider_slot!(
    emit_transcription_cost_rates_struct,
    super::schema::TranscriptionCostRates
);

#[cfg(test)]
mod canonical_registry_tests {
    use super::*;

    #[test]
    fn canonical_model_keys_keep_dots_as_exact_keys() {
        let providers: Providers = toml::from_str(
            r#"
[connections."gateway.prod"]
kind = "openai-compatible"
capabilities = ["chat"]
[connections."gateway.prod".auth]
mode = "api_key"
api_key = "test-key"

[models."chat.fast.us"]
capability = "chat"
provider = "gateway.prod"
model = "gpt-4o-mini"
"#,
        )
        .expect("canonical provider registry should deserialize");
        assert!(
            providers
                .models
                .canonical_model(&ModelRef::from("chat.fast.us"))
                .is_some()
        );
        assert!(
            providers
                .models
                .canonical_model(&ModelRef::from("chat"))
                .is_none()
        );
        let registry = providers
            .canonical_registry()
            .expect("canonical registry should validate");
        assert!(
            registry
                .selection(
                    &ModelRef::from("chat.fast.us"),
                    zeroclaw_api::capability::Capability::Chat
                )
                .is_ok()
        );
    }

    #[test]
    fn canonical_and_legacy_model_rows_round_trip_together() {
        let providers: Providers = toml::from_str(
            r#"
[connections."gateway.prod"]
kind = "openai-compatible"
capabilities = ["chat"]
[connections."gateway.prod".auth]
mode = "api_key"
api_key = "test-key"

[models.openai.work]
model = "gpt-4o"

[models."chat.fast.us"]
capability = "chat"
provider = "gateway.prod"
model = "gpt-4o-mini"
"#,
        )
        .expect("mixed provider rows should deserialize");

        assert!(providers.models.find("openai", "work").is_some());
        assert!(
            providers
                .models
                .canonical_model(&ModelRef::from("chat.fast.us"))
                .is_some()
        );

        let encoded = toml::to_string(&providers).expect("mixed provider rows should serialize");
        assert!(encoded.contains("[models.openai.work]"));
        assert!(encoded.contains("[models.\"chat.fast.us\"]"));
        let _: Providers = toml::from_str(&encoded).expect("mixed provider rows should reload");
    }
}
