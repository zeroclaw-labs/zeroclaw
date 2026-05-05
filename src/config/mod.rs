//! Binary-side config module. Pure re-export surface — the real types and
//! helpers live in `zeroclaw-config`. Everything the binary needs (schema,
//! traits, property helpers) is pulled through here so `crate::config::*`
//! continues to resolve for callers that predate the crate split.

pub use zeroclaw_config::migration;
pub use zeroclaw_config::providers;
pub mod schema;
pub mod traits;
pub mod workspace;

pub use schema::{
    AssemblyAiSttConfig, AuditConfig, BackupConfig, BrowserComputerUseConfig, BrowserConfig,
    BuiltinHooksConfig, ChannelsConfig, ClassificationRule, ClaudeCodeConfig,
    ClaudeCodeRunnerConfig, CloudOpsConfig, CodexCliConfig, ComposioConfig, Config,
    ConversationalAiConfig, CostConfig, CronJobDecl, CronScheduleDecl, DEFAULT_GWS_SERVICES,
    DataRetentionConfig, DeepgramSttConfig, DelegateAgentConfig, DelegateToolConfig, DiscordConfig,
    DockerRuntimeConfig, EmbeddingRouteConfig, EstopConfig, FeishuConfig, GatewayConfig,
    GeminiCliConfig, GoogleSttConfig, GoogleWorkspaceAllowedOperation, GoogleWorkspaceConfig,
    HardwareConfig, HardwareTransport, HeartbeatConfig, HooksConfig, HttpRequestConfig,
    IMessageConfig, IdentityConfig, ImageGenConfig, ImageProviderDalleConfig,
    ImageProviderFluxConfig, ImageProviderImagenConfig, ImageProviderStabilityConfig, JiraConfig,
    KnowledgeConfig, LarkConfig, LinkEnricherConfig, LinkedInConfig, LinkedInContentConfig,
    LinkedInImageConfig, LocalWhisperConfig, MatrixConfig, McpConfig, McpServerConfig,
    McpTransport, MediaPipelineConfig, MemoryConfig, MemoryPolicyConfig, Microsoft365Config,
    ModelRouteConfig, MqttConfig, MultimodalConfig, NextcloudTalkConfig, NodeTransportConfig,
    NodesConfig, NotionConfig, ObservabilityConfig, OpenAiSttConfig, OpenCodeCliConfig,
    OpenVpnTunnelConfig, OtpConfig, OtpMethod, PacingConfig, PeripheralBoardConfig,
    PeripheralsConfig, PipelineConfig, PluginsConfig, PostgresStorageConfig, ProjectIntelConfig,
    ProxyConfig, ProxyScope, QdrantStorageConfig, QueryClassificationConfig, ReliabilityConfig,
    ResourceLimitsConfig, RiskProfileConfig, RuntimeConfig, SandboxBackend, SandboxConfig,
    SchedulerConfig, SearchMode, SecretsConfig, SecurityConfig, SecurityOpsConfig, ShellToolConfig,
    SkillCreationConfig, SkillImprovementConfig, SkillsConfig, SkillsPromptInjectionMode,
    SlackConfig, SopConfig, SqliteStorageConfig, StorageConfig, StreamMode, TelegramConfig,
    TextBrowserConfig, ToolFilterGroup, ToolFilterGroupMode, TranscriptionConfig, TtsConfig,
    TtsProviderConfig, TunnelConfig, VerifiableIntentConfig, WebFetchConfig, WebSearchConfig,
    WebhookConfig, WhatsAppChatPolicy, WhatsAppWebMode, WorkspaceConfig,
    apply_channel_proxy_to_builder, apply_runtime_proxy_to_builder, build_channel_proxy_client,
    build_channel_proxy_client_with_timeouts, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, runtime_proxy_config, set_runtime_proxy_config,
    ws_connect_with_proxy,
};

pub use schema::ModelProviderConfig;
// Per-family model provider configs (typed split — #6273). Re-exported here
// so tests + downstream binary callers can construct typed family entries
// without reaching into `zeroclaw_config::schema` directly.
pub use schema::{
    Ai21ModelProviderConfig, AihubmixModelProviderConfig, AnthropicModelProviderConfig,
    AnyscaleModelProviderConfig, AstraiModelProviderConfig, AvianModelProviderConfig,
    AzureModelProviderConfig, BaichuanModelProviderConfig, BasetenModelProviderConfig,
    BedrockModelProviderConfig, CerebrasModelProviderConfig, CloudflareModelProviderConfig,
    CohereModelProviderConfig, CopilotModelProviderConfig, CustomModelProviderConfig,
    DeepinfraModelProviderConfig, DeepmystModelProviderConfig, DeepseekModelProviderConfig,
    DoubaoModelProviderConfig, FireworksModelProviderConfig, FriendliModelProviderConfig,
    GeminiCliModelProviderConfig, GeminiModelProviderConfig, GlmModelProviderConfig,
    GroqModelProviderConfig, HuggingfaceModelProviderConfig, HunyuanModelProviderConfig,
    HyperbolicModelProviderConfig, KiloCliModelProviderConfig, LeptonModelProviderConfig,
    LitellmModelProviderConfig, LlamacppModelProviderConfig, LmstudioModelProviderConfig,
    MinimaxModelProviderConfig, MistralModelProviderConfig, MoonshotModelProviderConfig,
    NebiusModelProviderConfig, NovitaModelProviderConfig, NscaleModelProviderConfig,
    NvidiaModelProviderConfig, OllamaModelProviderConfig, OpenAIModelProviderConfig,
    OpenRouterModelProviderConfig, OpencodeModelProviderConfig, OsaurusModelProviderConfig,
    OvhModelProviderConfig, PerplexityModelProviderConfig, QianfanModelProviderConfig,
    QwenModelProviderConfig, RekaModelProviderConfig, SambanovaModelProviderConfig,
    SglangModelProviderConfig, SiliconflowModelProviderConfig, StepfunModelProviderConfig,
    SyntheticModelProviderConfig, TelnyxModelProviderConfig, TogetherModelProviderConfig,
    VeniceModelProviderConfig, VercelModelProviderConfig, VllmModelProviderConfig,
    XaiModelProviderConfig, YiModelProviderConfig, ZaiModelProviderConfig,
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

    #[test]
    fn reexported_channel_configs_are_constructible() {
        let telegram = TelegramConfig {
            bot_token: "token".into(),
            allowed_users: vec!["alice".into()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
            excluded_tools: vec![],
        };

        let discord = DiscordConfig {
            bot_token: "token".into(),
            guild_ids: vec!["123".into()],
            channel_ids: vec![],
            archive: false,
            allowed_users: vec![],
            listen_to_bots: false,
            interrupt_on_new_message: false,
            mention_only: false,
            proxy_url: None,
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            multi_message_delay_ms: 800,
            stall_timeout_secs: 0,
            approval_timeout_secs: 300,
            excluded_tools: vec![],
        };

        let lark = LarkConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            mention_only: false,
            use_feishu: false,
            receive_mode: crate::config::schema::LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
            excluded_tools: vec![],
        };
        let feishu = FeishuConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            mention_only: false,
            receive_mode: crate::config::schema::LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
            excluded_tools: vec![],
        };

        let nextcloud_talk = NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: None,
            allowed_users: vec!["*".into()],
            proxy_url: None,
            bot_name: None,
            excluded_tools: vec![],
        };

        assert_eq!(telegram.allowed_users.len(), 1);
        assert_eq!(discord.guild_ids, vec!["123".to_string()]);
        assert_eq!(lark.app_id, "app-id");
        assert_eq!(feishu.app_id, "app-id");
        assert_eq!(nextcloud_talk.base_url, "https://cloud.example.com");
    }
}
