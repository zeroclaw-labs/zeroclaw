pub mod schema;
pub mod traits;
pub mod workspace;

#[allow(unused_imports)]
pub use schema::{
    apply_channel_proxy_to_builder, apply_runtime_proxy_to_builder, build_channel_proxy_client,
    build_channel_proxy_client_with_timeouts, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, runtime_proxy_config, set_runtime_proxy_config,
    AgentConfig, AssemblyAiSttConfig, AuditConfig, AutonomyConfig, BackupConfig,
    BrowserComputerUseConfig, BrowserConfig, BuiltinHooksConfig, ChannelsConfig,
    ClassificationRule, ClaudeCodeConfig, CloudOpsConfig, ComposioConfig, Config,
    ConversationalAiConfig, CostConfig, CronConfig, CronJobDecl, CronScheduleDecl,
    DataRetentionConfig, DeepgramSttConfig, DelegateAgentConfig, DelegateToolConfig, DiscordConfig,
    DockerRuntimeConfig, EdgeTtsConfig, ElevenLabsTtsConfig, EmbeddingRouteConfig, EstopConfig,
    FeishuConfig, GatewayConfig, GoogleSttConfig, GoogleTtsConfig, GoogleWorkspaceAllowedOperation,
    GoogleWorkspaceConfig, HardwareConfig, HardwareTransport, HeartbeatConfig, HooksConfig,
    HttpRequestConfig, IMessageConfig, IdentityConfig, ImageGenConfig, ImageProviderDalleConfig,
    ImageProviderFluxConfig, ImageProviderImagenConfig, ImageProviderStabilityConfig, JiraConfig,
    KnowledgeConfig, LarkConfig, LinkedInConfig, LinkedInContentConfig, LinkedInImageConfig,
    LocalWhisperConfig, MatrixConfig, McpConfig, McpServerConfig, McpTransport, MemoryConfig,
    MemoryPolicyConfig, Microsoft365Config, ModelRouteConfig, MultimodalConfig,
    NextcloudTalkConfig, NodeTransportConfig, NodesConfig, NotionConfig, ObservabilityConfig,
    OpenAiSttConfig, OpenAiTtsConfig, OpenVpnTunnelConfig, OtpConfig, OtpMethod, PacingConfig,
    PeripheralBoardConfig, PeripheralsConfig, PiperTtsConfig, PluginsConfig, ProjectIntelConfig,
    ProxyConfig, ProxyScope, QdrantConfig, QueryClassificationConfig, ReliabilityConfig,
    ResourceLimitsConfig, RuntimeConfig, SandboxBackend, SandboxConfig, SchedulerConfig,
    SecretsConfig, SecurityConfig, SecurityOpsConfig, SkillCreationConfig, SkillsConfig,
    SkillsPromptInjectionMode, SlackConfig, StorageConfig, StorageProviderConfig,
    StorageProviderSection, StreamMode, SwarmConfig, SwarmStrategy, TelegramConfig,
    TextBrowserConfig, ToolFilterGroup, ToolFilterGroupMode, TranscriptionConfig, TtsConfig,
    TunnelConfig, VerifiableIntentConfig, WebFetchConfig, WebSearchConfig, WebhookConfig,
    WhatsAppChatPolicy, WhatsAppWebMode, WorkspaceConfig, DEFAULT_GWS_SERVICES,
};

pub fn name_and_presence<T: traits::ChannelConfig>(channel: Option<&T>) -> (&'static str, bool) {
    (T::name(), channel.is_some())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reexported_config_default_is_constructible() {
        let config = Config::default();

        assert!(config.default_provider.is_some());
        assert!(config.default_model.is_some());
        assert!(config.default_temperature > 0.0);
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
        };

        let discord = DiscordConfig {
            bot_token: "token".into(),
            guild_id: Some("123".into()),
            allowed_users: vec![],
            listen_to_bots: false,
            interrupt_on_new_message: false,
            mention_only: false,
            proxy_url: None,
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
        };
        let feishu = FeishuConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            receive_mode: crate::config::schema::LarkReceiveMode::Websocket,
            port: None,
            proxy_url: None,
        };

        let nextcloud_talk = NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: None,
            allowed_users: vec!["*".into()],
            proxy_url: None,
        };

        assert_eq!(telegram.allowed_users.len(), 1);
        assert_eq!(discord.guild_id.as_deref(), Some("123"));
        assert_eq!(lark.app_id, "app-id");
        assert_eq!(feishu.app_id, "app-id");
        assert_eq!(nextcloud_talk.base_url, "https://cloud.example.com");
    }
}
