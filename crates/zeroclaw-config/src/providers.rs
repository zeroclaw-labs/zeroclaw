use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use zeroclaw_macros::Configurable;

use super::schema::{
    Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnthropicModelProviderConfig,
    AnyscaleModelProviderConfig, ArceeModelProviderConfig, AstraiModelProviderConfig,
    AtomicChatModelProviderConfig, AvianModelProviderConfig, AzureModelProviderConfig,
    BaichuanModelProviderConfig, BasetenModelProviderConfig, BedrockModelProviderConfig,
    CerebrasModelProviderConfig, CloudflareModelProviderConfig, CohereModelProviderConfig,
    CopilotModelProviderConfig, CustomModelProviderConfig, DeepinfraModelProviderConfig,
    DeepmystModelProviderConfig, DeepseekModelProviderConfig, DoubaoModelProviderConfig,
    FeatherlessModelProviderConfig, FireworksModelProviderConfig, FriendliModelProviderConfig,
    GeminiCliModelProviderConfig, GeminiModelProviderConfig, GithubModelsModelProviderConfig,
    GlmModelProviderConfig, GroqModelProviderConfig, HuggingfaceModelProviderConfig,
    HunyuanModelProviderConfig, HyperbolicModelProviderConfig, InceptionModelProviderConfig,
    KiloCliModelProviderConfig, KiloModelProviderConfig, LambdaAiModelProviderConfig,
    LeptonModelProviderConfig, LitellmModelProviderConfig, LlamacppModelProviderConfig,
    LmstudioModelProviderConfig, MinimaxModelProviderConfig, MistralModelProviderConfig,
    ModelProviderConfig, MoonshotModelProviderConfig, MorphModelProviderConfig,
    NebiusModelProviderConfig, NovitaModelProviderConfig, NscaleModelProviderConfig,
    NvidiaModelProviderConfig, OllamaModelProviderConfig, OpenAIModelProviderConfig,
    OpenRouterModelProviderConfig, OpencodeModelProviderConfig, OsaurusModelProviderConfig,
    OvhModelProviderConfig, PerplexityModelProviderConfig, QianfanModelProviderConfig,
    QwenModelProviderConfig, RekaModelProviderConfig, SambanovaModelProviderConfig,
    SglangModelProviderConfig, SiliconflowModelProviderConfig, StepfunModelProviderConfig,
    SyntheticModelProviderConfig, TelnyxModelProviderConfig, TogetherModelProviderConfig,
    UpstageModelProviderConfig, VeniceModelProviderConfig, VercelModelProviderConfig,
    VllmModelProviderConfig, XaiModelProviderConfig, YiModelProviderConfig, ZaiModelProviderConfig,
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

#[macro_export]
macro_rules! define_provider_ref {
    ($name:ident, $category_doc:literal) => {
        #[doc = concat!("Reference to a configured `[", $category_doc, ".<type>.<alias>]` entry.")]
        ///
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

/// Hard ceiling on `providers.models.<alias>.fallback` chain depth. The cycle
/// guard only bounds chains that loop; a long acyclic chain would otherwise
/// recurse one stack frame per alias at config-load and build time, turning a
/// pathological config into a startup stack overflow. Both the validation walk
/// and the runtime build walk stop descending past this depth and prune the
/// rest of the branch.
pub const MAX_FALLBACK_DEPTH: usize = 3;

/// Macro that expands to a single source of truth for the per-provider-type
/// slot list on `ModelProviders`. Every helper that needs to walk every slot
/// (`find`, `iter_entries`, `is_empty`, etc.) goes through this
/// macro so adding a new model_provider type is a one-line addition here, not a
/// shotgun edit across multiple helpers.
///
/// Each row is `(field_ident, provider_type_str, FamilyConfigType)`. The
/// `provider_type_str` is the canonical TOML outer key, identical to the
/// field name with hyphens forbidden (the schema uses underscores).
///
/// Exported so that downstream crates (notably `zeroclaw-providers`) can
/// drive their own dispatch from the same single source of truth — adding
/// a family is one row here and one trait impl in providers; missing the
/// impl fails to compile when downstream macro consumers expand.
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
            (gemini, "gemini", GeminiModelProviderConfig),
            (gemini_cli, "gemini_cli", GeminiCliModelProviderConfig),
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
            (custom, "custom", CustomModelProviderConfig),
        }
    };
}

macro_rules! emit_model_providers_struct {
    ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
        /// Typed model provider container — one slot per canonical model_provider type.
        ///
        /// Replaces the `HashMap<String, HashMap<String, ModelProviderConfig>>`
        /// with a typed struct so each family's per-alias map carries its own
        /// typed config (with the family's `*Endpoint` enum and family-specific
        /// extras visible at the type level).
        ///
        /// TOML shape is preserved byte-identical: each named field deserializes
        /// from the same `[providers.models.<type>.<alias>]` block as before.
        ///
        /// Adding a new model_provider family means: define the typed config in
        /// `schema.rs`, then add one row to `for_each_model_provider_slot!`,
        /// and every helper picks up the new slot automatically.
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

    /// Resolve a name that is either a bare `<alias>` or a `<kind>.<alias>` pair
    /// to its `(kind, alias, &config)`. A bare alias is matched across every
    /// family; ambiguity (same alias under multiple kinds) returns `None` so the
    /// caller can ask the user to qualify it. Registry-driven via
    /// `for_each_model_provider_slot!`.
    pub fn find_by_name(&self, name: &str) -> Option<(&'static str, String, &ModelProviderConfig)> {
        if let Some((kind, alias)) = name.split_once('.') {
            macro_rules! emit_dotted {
                ($(($field:ident, $type_str:literal, $cfg_ty:ty)),+ $(,)?) => {
                    match kind {
                        $( $type_str => self.$field.get(alias).map(|c| ($type_str, alias.to_string(), &c.base)), )+
                        _ => None,
                    }
                };
            }
            return for_each_model_provider_slot!(emit_dotted);
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

/// Top-level wrapper for every provider category. TOML root sees a
/// single `[providers]` table with one sub-key per category:
///
/// ```toml
/// [providers.models.anthropic.default]
/// api_key = "..."
///
/// [providers.tts.openai.default]
/// api_key = "..."
///
/// [providers.transcription.groq.default]
/// api_key = "..."
/// ```
///
/// Each category keeps its own typed-slot internals (so per-family
/// endpoints and extras stay validated at the type level); this
/// wrapper just gives them a shared top-level home.
#[derive(Debug, Clone, Default, Serialize, Deserialize, Configurable)]
#[cfg_attr(feature = "schema-export", derive(schemars::JsonSchema))]
#[prefix = "providers"]
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
}

// ── Cost-rate wrappers ──────────────────────────────────────────────────────
//
// Same per-provider-type slot layout as the typed-provider wrappers above,
// but the value type is the per-resource rate struct instead of the
// per-alias provider config. Each subsection's TOML path mirrors its
// `[providers.*]` counterpart with the trailing `<alias>` segment replaced
// by the resource the rate prices (model id, voice id, etc.).
//
// DRY:
//   - `ModelCostRatesByProvider` consumes the same `for_each_model_provider_slot!`
//     macro as `ModelProviders`, so adding a new provider type updates
//     both structs from a single edit.
//   - `TtsCostRatesByProvider` and `TranscriptionCostRatesByProvider`
//     mirror their `TtsProviders` / `TranscriptionProviders` slot lists
//     by hand (those wrappers are themselves hand-rolled because the
//     closed family lists were small enough to not warrant a macro).

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
