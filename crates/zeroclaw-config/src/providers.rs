use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroclaw_macros::Configurable;

use super::schema::{
    Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnthropicModelProviderConfig,
    AnyscaleModelProviderConfig, AstraiModelProviderConfig, AvianModelProviderConfig,
    AzureModelProviderConfig, BaichuanModelProviderConfig, BasetenModelProviderConfig,
    BedrockModelProviderConfig, CerebrasModelProviderConfig, CloudflareModelProviderConfig,
    CohereModelProviderConfig, CopilotModelProviderConfig, CustomModelProviderConfig,
    DeepinfraModelProviderConfig, DeepmystModelProviderConfig, DeepseekModelProviderConfig,
    DoubaoModelProviderConfig, EmbeddingRouteConfig, FireworksModelProviderConfig,
    FriendliModelProviderConfig, GeminiCliModelProviderConfig, GeminiModelProviderConfig,
    GlmModelProviderConfig, GroqModelProviderConfig, HuggingfaceModelProviderConfig,
    HunyuanModelProviderConfig, HyperbolicModelProviderConfig, KiloCliModelProviderConfig,
    LeptonModelProviderConfig, LitellmModelProviderConfig, LlamacppModelProviderConfig,
    LmstudioModelProviderConfig, MinimaxModelProviderConfig, MistralModelProviderConfig,
    ModelProviderConfig, ModelRouteConfig, MoonshotModelProviderConfig, NebiusModelProviderConfig,
    NovitaModelProviderConfig, NscaleModelProviderConfig, NvidiaModelProviderConfig,
    OllamaModelProviderConfig, OpenAIModelProviderConfig, OpenRouterModelProviderConfig,
    OpencodeModelProviderConfig, OsaurusModelProviderConfig, OvhModelProviderConfig,
    PerplexityModelProviderConfig, QianfanModelProviderConfig, QwenModelProviderConfig,
    RekaModelProviderConfig, SambanovaModelProviderConfig, SglangModelProviderConfig,
    SiliconflowModelProviderConfig, StepfunModelProviderConfig, SyntheticModelProviderConfig,
    TelnyxModelProviderConfig, TogetherModelProviderConfig, VeniceModelProviderConfig,
    VercelModelProviderConfig, VllmModelProviderConfig, XaiModelProviderConfig,
    YiModelProviderConfig, ZaiModelProviderConfig,
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

// ── Per-category typed alias-ref newtypes ────────────────────────────────
//
// Every per-agent provider field is a reference into a specific configured
// `[providers.<category>.<type>.<alias>]` (or `[channels.<type>.<alias>]`)
// entry. The newtype carries the category at the type level — readers know
// `agent.tts_provider: TtsProviderRef` is a TTS-provider reference, not a
// free string, just by looking at the field declaration.
//
// `#[serde(transparent)]` keeps the on-disk TOML shape identical to the
// previous `String` field. `Deref<Target = str>` and `AsRef<str>` keep
// every `.is_empty()` / `.split_once('.')` / `.eq_ignore_ascii_case` /
// `&value[..]` consumer working unchanged. Assignment from a string literal
// goes through `.into()` (`From<&str>` / `From<String>`).
//
// Validation that each non-empty ref resolves to a configured alias lives
// in `Config::validate()` (see `agent.tts_provider` / `agent.transcription_provider`
// blocks in schema.rs); the newtype's job is to encode the *category* in
// the type, not the existence — both layers reinforce each other.

macro_rules! define_provider_ref {
    ($name:ident, $category_doc:literal) => {
        #[doc = concat!("Reference to a configured `[", $category_doc, ".<type>.<alias>]` entry.")]
        ///
        /// Empty value means "no preference" (opt-out). Non-empty values must
        /// resolve to a configured alias; `Config::validate()` enforces this.
        #[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Macro that expands to a single source of truth for the per-model_provider-type
/// slot list on `ModelProviders`. Every helper that needs to walk every slot
/// (`first_model_provider`, `iter_entries`, `is_empty`, etc.) goes through this
/// macro so adding a new model_provider type is a one-line addition here, not a
/// shotgun edit across multiple helpers.
///
/// Each row is `(field_ident, provider_type_str, FamilyConfigType)`. The
/// `provider_type_str` is the canonical TOML outer key, identical to the
/// field name with hyphens forbidden (the schema uses underscores).
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
            (gemini, "gemini", GeminiModelProviderConfig),
            (gemini_cli, "gemini_cli", GeminiCliModelProviderConfig),
            (bedrock, "bedrock", BedrockModelProviderConfig),
            (telnyx, "telnyx", TelnyxModelProviderConfig),
            (together, "together", TogetherModelProviderConfig),
            (fireworks, "fireworks", FireworksModelProviderConfig),
            (groq, "groq", GroqModelProviderConfig),
            (mistral, "mistral", MistralModelProviderConfig),
            (deepseek, "deepseek", DeepseekModelProviderConfig),
            (cohere, "cohere", CohereModelProviderConfig),
            (perplexity, "perplexity", PerplexityModelProviderConfig),
            (xai, "xai", XaiModelProviderConfig),
            (cerebras, "cerebras", CerebrasModelProviderConfig),
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
            (novita, "novita", NovitaModelProviderConfig),
            (nvidia, "nvidia", NvidiaModelProviderConfig),
            (vercel, "vercel", VercelModelProviderConfig),
            (cloudflare, "cloudflare", CloudflareModelProviderConfig),
            (ovh, "ovh", OvhModelProviderConfig),
            (copilot, "copilot", CopilotModelProviderConfig),
            (lmstudio, "lmstudio", LmstudioModelProviderConfig),
            (llamacpp, "llamacpp", LlamacppModelProviderConfig),
            (sglang, "sglang", SglangModelProviderConfig),
            (vllm, "vllm", VllmModelProviderConfig),
            (osaurus, "osaurus", OsaurusModelProviderConfig),
            (litellm, "litellm", LitellmModelProviderConfig),
            (lepton, "lepton", LeptonModelProviderConfig),
            (synthetic, "synthetic", SyntheticModelProviderConfig),
            (opencode, "opencode", OpencodeModelProviderConfig),            (kilocli, "kilocli", KiloCliModelProviderConfig),
            (custom, "custom", CustomModelProviderConfig),
        }
    };
}

macro_rules! emit_model_providers_struct {
    ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
        /// Typed model-model_provider container — one slot per canonical model_provider type.
        ///
        /// Replaces the V3 `HashMap<String, HashMap<String, ModelProviderConfig>>`
        /// with a typed struct so each family's per-alias map carries its own
        /// typed config (with the family's `*Endpoint` enum and family-specific
        /// extras visible at the type level).
        ///
        /// TOML shape is preserved byte-identical: each named field deserializes
        /// from the same `[providers.models.<type>.<alias>]` block as before.
        ///
        /// Adding a new model_provider family means: define the typed config in
        /// `schema.rs`, then add one row to `for_each_model_provider_slot!` —
        /// every helper picks up the new slot automatically.
        #[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
        #[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
        #[prefix = "providers.models"]
        pub struct ModelProviders {
            $(
                #[serde(default, skip_serializing_if = "HashMap::is_empty")]
                #[nested]
                pub $field: HashMap<String, $cfg_ty>,
            )+
        }
    };
}
for_each_model_provider_slot!(emit_model_providers_struct);

impl ModelProviders {
    /// Iterate every entry across every typed slot, yielding
    /// `(provider_type, alias, &base)` triples. Use this when consumer code
    /// needs to walk every model model_provider entry without caring about family.
    ///
    /// Materializes through a `Vec` rather than chaining iterators directly:
    /// with ~60 typed slots the deeply-nested `Chain<Chain<...>>` type blows
    /// up rustc's `Freeze` trait-resolution recursion limit. The collection
    /// cost is negligible (entries are sparse — most slots are empty in any
    /// real config). Returned as `impl Iterator` so call sites can chain
    /// `.next()`, `.filter_map()`, etc. without changes.
    pub fn iter_entries(&self) -> impl Iterator<Item = (&'static str, &str, &ModelProviderConfig)> {
        let mut out: Vec<(&'static str, &str, &ModelProviderConfig)> = Vec::new();
        macro_rules! emit_iter {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $(
                    for (alias, cfg) in &self.$field {
                        out.push(($type_str, alias.as_str(), &cfg.base));
                    }
                )+
            };
        }
        for_each_model_provider_slot!(emit_iter);
        out.into_iter()
    }

    /// Iterate every entry mutably across every typed slot.
    pub fn iter_entries_mut(
        &mut self,
    ) -> impl Iterator<Item = (&'static str, &str, &mut ModelProviderConfig)> {
        let mut out: Vec<(&'static str, &str, &mut ModelProviderConfig)> = Vec::new();
        macro_rules! emit_iter_mut {
            ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                $(
                    for (alias, cfg) in self.$field.iter_mut() {
                        out.push(($type_str, alias.as_str(), &mut cfg.base));
                    }
                )+
            };
        }
        for_each_model_provider_slot!(emit_iter_mut);
        out.into_iter()
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

    /// Get-or-create the shared base config for a `<provider_type>.<alias>`
    /// pair, returning a mutable reference. Used by tools that mutate
    /// generic baseline fields (model, temperature, api_key) without caring
    /// about the family's specific extras. Returns `None` for unknown
    /// model_provider types.
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

/// Typed TTS-model_provider container — one slot per TTS family. Mirrors
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
}

/// Top-level `[providers]` section. Wraps model provider, TTS provider,
/// transcription provider profiles and routing rules.
#[derive(Debug, Clone, Serialize, Deserialize, Configurable, Default)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "model_providers"]
pub struct ProvidersConfig {
    /// Named model model_provider profiles: outer key = model_provider type, inner key = user alias.
    /// V3 shape: `[providers.models.<type>.<alias>]` e.g. `[providers.models.anthropic.default]`.
    /// Typed via `ModelProviders` — every model_provider type has its own typed slot
    /// carrying the family's `*Endpoint` enum and family-specific extras.
    #[serde(default)]
    #[nested]
    pub models: ModelProviders,

    /// Named TTS provider profiles: outer key = provider family, inner key = user alias.
    /// V3 shape: `[providers.tts.<type>.<alias>]` e.g. `[providers.tts.openai.<alias>]`.
    /// Mirrors `models` with the typed-family split: each TTS family has its
    /// own slot carrying its `*TtsEndpoint` enum.
    #[serde(default)]
    #[nested]
    pub tts: TtsProviders,

    /// Named transcription / STT provider profiles: outer key = provider family,
    /// inner key = user alias. V3 shape:
    /// `[providers.transcription.<type>.<alias>]` e.g.
    /// `[providers.transcription.groq.<alias>]`. Six family slots: `groq`,
    /// `openai`, `deepgram`, `assemblyai`, `google`, `local_whisper`.
    /// Mirrors `models` and `tts`. Per-agent reference via
    /// `agent.transcription_provider = "groq.<alias>"` (resolved at validation
    /// time against this section's configured aliases).
    #[serde(default)]
    #[nested]
    pub transcription: TranscriptionProviders,

    /// Model routing rules — route `hint:<name>` to specific model provider + model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub model_routes: Vec<ModelRouteConfig>,

    /// Embedding routing rules — route `hint:<name>` to specific model_provider+model combos.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub embedding_routes: Vec<EmbeddingRouteConfig>,
}

impl ProvidersConfig {
    /// Return the first concrete `model` string available for use as a default.
    ///
    /// Scans every typed slot's entries (iteration order is the macro slot
    /// order) for one that has `model` set.
    ///
    /// Returns `None` only when no model_provider entry has any model configured at all.
    pub fn resolve_default_model(&self) -> Option<String> {
        self.models
            .iter_entries()
            .filter_map(|(_, _, base)| base.model.as_deref().map(str::trim))
            .find(|m| !m.is_empty())
            .map(ToString::to_string)
    }

    /// Return the first `ModelProviderConfig` (the shared base) from `models`,
    /// if any exists.
    pub fn first_model_provider(&self) -> Option<&ModelProviderConfig> {
        self.models.iter_entries().next().map(|(_, _, base)| base)
    }

    /// Return a mutable reference to the first `ModelProviderConfig` (the
    /// shared base) from `models`, if any exists.
    pub fn first_model_provider_mut(&mut self) -> Option<&mut ModelProviderConfig> {
        self.models
            .iter_entries_mut()
            .next()
            .map(|(_, _, base)| base)
    }

    /// Return the model_provider type key of the first entry in `models`, if any.
    /// Use this when callers need the bare type name (e.g. model_provider routing
    /// factories that take `"openrouter"` not `"openrouter.default"`).
    pub fn first_model_provider_type(&self) -> Option<&'static str> {
        self.models.iter_entries().next().map(|(ty, _, _)| ty)
    }

    /// Return the V3 dotted `<type>.<alias>` identifier of the first
    /// configured model model_provider entry, if any. Use this when callers need
    /// the V3 alias reference (matches `agents.<x>.model_provider` values).
    pub fn first_provider_alias(&self) -> Option<String> {
        self.models
            .iter_entries()
            .next()
            .map(|(ty, alias, _)| format!("{ty}.{alias}"))
    }
}
