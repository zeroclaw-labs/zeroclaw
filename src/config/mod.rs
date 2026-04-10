pub mod schema;
pub mod traits;
pub mod workspace;

#[allow(unused_imports)]
pub use schema::{
    AgentConfig, AssemblyAiSttConfig, AuditConfig, AutonomyConfig, BackupConfig,
    BrowserComputerUseConfig, BrowserConfig, BuiltinHooksConfig, ChannelsConfig,
    ClassificationRule, ClaudeCodeConfig, ClaudeCodeRunnerConfig, CloudOpsConfig, CodexCliConfig,
    ComposioConfig, Config, ConversationalAiConfig, CostConfig, CronConfig, CronJobDecl,
    CronScheduleDecl, DEFAULT_GWS_SERVICES, DataRetentionConfig, DeepgramSttConfig,
    DelegateAgentConfig, DelegateToolConfig, DockerRuntimeConfig, EdgeTtsConfig,
    ElevenLabsTtsConfig, EmbeddingRouteConfig, EstopConfig, GatewayConfig, GeminiCliConfig,
    GoogleSttConfig, GoogleTtsConfig, GoogleWorkspaceAllowedOperation, GoogleWorkspaceConfig,
    HeartbeatConfig, HooksConfig, HttpRequestConfig, IdentityConfig, ImageGenConfig,
    ImageProviderDalleConfig, ImageProviderFluxConfig, ImageProviderImagenConfig,
    ImageProviderStabilityConfig, JiraConfig, KnowledgeConfig, LinkEnricherConfig, LinkedInConfig,
    LinkedInContentConfig, LinkedInImageConfig, LocalWhisperConfig, McpConfig, McpServerConfig,
    McpTransport, MediaPipelineConfig, MemoryConfig, MemoryPolicyConfig, Microsoft365Config,
    ModelRouteConfig, MultimodalConfig, NodeTransportConfig, NodesConfig, NotionConfig,
    ObservabilityConfig, OpenAiSttConfig, OpenAiTtsConfig, OpenCodeCliConfig, OpenVpnTunnelConfig,
    OtpConfig, OtpMethod, PacingConfig, PipelineConfig, PiperTtsConfig, PluginsConfig,
    ProjectIntelConfig, ProxyConfig, ProxyScope, QdrantConfig, QueryClassificationConfig,
    ReliabilityConfig, ResourceLimitsConfig, RuntimeConfig, SandboxBackend, SandboxConfig,
    SchedulerConfig, SearchMode, SecretsConfig, SecurityConfig, SecurityOpsConfig, ShellToolConfig,
    SkillCreationConfig, SkillImprovementConfig, SkillsConfig, SkillsPromptInjectionMode,
    SlackConfig, SopConfig, StorageConfig, StorageProviderConfig, StorageProviderSection,
    StreamMode, SwarmConfig, SwarmStrategy, TelegramConfig, TextBrowserConfig, ToolFilterGroup,
    ToolFilterGroupMode, TranscriptionConfig, TtsConfig, TunnelConfig, VerifiableIntentConfig,
    WebFetchConfig, WebSearchConfig, WorkspaceConfig, apply_channel_proxy_to_builder,
    apply_runtime_proxy_to_builder, build_channel_proxy_client,
    build_channel_proxy_client_with_timeouts, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, runtime_proxy_config, set_runtime_proxy_config,
    ws_connect_with_proxy,
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

        let slack: SlackConfig = serde_json::from_str(r#"{"bot_token":"xoxb-tok"}"#).unwrap();

        assert_eq!(telegram.allowed_users.len(), 1);
        assert_eq!(slack.bot_token, "xoxb-tok");
    }
}
