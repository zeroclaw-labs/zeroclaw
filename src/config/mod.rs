//! Binary-side config module. Pure re-export surface — the real types and
//! helpers live in `zeroclaw-config`. Everything the binary needs (schema,
//! traits, property helpers) is pulled through here so `crate::config::*`
//! continues to resolve for callers that predate the crate split.

pub use zeroclaw_config::migration;
pub use zeroclaw_config::providers;
pub use zeroclaw_http::{
    apply_channel_proxy_to_builder, apply_persisted_proxy_on_boot, apply_runtime_proxy_to_builder,
    build_channel_proxy_client, build_channel_proxy_client_with_timeouts,
    build_runtime_proxy_client, build_runtime_proxy_client_with_timeouts, runtime_proxy_config,
    set_runtime_proxy_config,
};
pub mod schema;
pub mod traits;

pub use schema::{
    AliasedAgentConfig, AssemblyAiSttConfig, AuditConfig, BackupConfig, BrowserComputerUseConfig,
    BrowserConfig, BuiltinHooksConfig, ChannelsConfig, ClassificationRule, Config, CostConfig,
    CronJobDecl, CronScheduleDecl, DataRetentionConfig, DeepgramSttConfig, DiscordConfig,
    DockerRuntimeConfig, EmbeddingRouteConfig, EstopConfig, GatewayConfig, GoogleSttConfig,
    HeartbeatConfig, HooksConfig, HttpRequestConfig, IdentityConfig, ImageGenConfig,
    LinkEnricherConfig, LocalWhisperConfig, McpConfig, McpServerConfig, McpTransport,
    MediaPipelineConfig, MemoryConfig, MemoryPolicyConfig, ModelRouteConfig, MultimodalConfig,
    NodeTransportConfig, NodesConfig, ObservabilityConfig, OpenAiSttConfig, OpenVpnTunnelConfig,
    OtpConfig, OtpMethod, PacingConfig, PipelineConfig, ProxyConfig, ProxyScope,
    QueryClassificationConfig, ReliabilityConfig, RiskProfileConfig, RuntimeConfig, SandboxBackend,
    SandboxConfig, SchedulerConfig, SearchMode, SecretsConfig, SecurityConfig, SecurityOpsConfig,
    ShellToolConfig, SkillCreationConfig, SkillImprovementConfig, SkillsConfig,
    SkillsPromptInjectionMode, SopConfig, StorageConfig, StreamMode, TelegramConfig,
    TextBrowserConfig, ToolFilterGroup, ToolFilterGroupMode, TranscriptionConfig, TtsConfig,
    TtsProviderConfig, TunnelConfig, VerifiableIntentConfig, WebFetchConfig, WebSearchConfig,
    WebhookConfig, read_persisted_proxy_for_boot,
};

pub use schema::ModelProviderConfig;
// Per-family model model_provider configs (typed split — Re-exported here
// so tests + downstream binary callers can construct typed family entries
// without reaching into `zeroclaw_config::schema` directly.
pub use schema::{
    Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnthropicModelProviderConfig,
    AnyscaleModelProviderConfig, AstraiModelProviderConfig, AvianModelProviderConfig,
    BaichuanModelProviderConfig, BasetenModelProviderConfig, CerebrasModelProviderConfig,
    CloudflareModelProviderConfig, CohereModelProviderConfig, CopilotModelProviderConfig,
    CustomModelProviderConfig, DeepinfraModelProviderConfig, DeepmystModelProviderConfig,
    DeepseekModelProviderConfig, DoubaoModelProviderConfig, FireworksModelProviderConfig,
    FriendliModelProviderConfig, GeminiModelProviderConfig, GlmModelProviderConfig,
    GroqModelProviderConfig, HuggingfaceModelProviderConfig, HunyuanModelProviderConfig,
    HyperbolicModelProviderConfig, LeptonModelProviderConfig, LitellmModelProviderConfig,
    LlamacppModelProviderConfig, LmstudioModelProviderConfig, MinimaxModelProviderConfig,
    MistralModelProviderConfig, MoonshotModelProviderConfig, NebiusModelProviderConfig,
    NovitaModelProviderConfig, NscaleModelProviderConfig, NvidiaModelProviderConfig,
    OllamaModelProviderConfig, OpenAIModelProviderConfig, OpenRouterModelProviderConfig,
    OpencodeModelProviderConfig, OsaurusModelProviderConfig, OvhModelProviderConfig,
    PerplexityModelProviderConfig, QianfanModelProviderConfig, QwenModelProviderConfig,
    RekaModelProviderConfig, SambanovaModelProviderConfig, SglangModelProviderConfig,
    SiliconflowModelProviderConfig, StepfunModelProviderConfig, SyntheticModelProviderConfig,
    TogetherModelProviderConfig, VeniceModelProviderConfig, VercelModelProviderConfig,
    VllmModelProviderConfig, XaiModelProviderConfig, YiModelProviderConfig, ZaiModelProviderConfig,
};
pub use traits::HasPropKind;
pub use traits::PropFieldInfo;
pub use traits::PropKind;
pub use traits::SecretFieldInfo;

// Property helpers — single source of truth in zeroclaw-config.
#[cfg(feature = "schema-export")]
pub use zeroclaw_config::helpers::enum_variants;
pub use zeroclaw_config::helpers::{
    make_prop_field, route_hashmap_path, serde_get_prop, serde_set_prop,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_config_default_is_constructible() {
        let config = Config::default();

        assert!(config.providers.models.is_empty());
    }
}
