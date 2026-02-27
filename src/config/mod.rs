pub mod schema;
pub mod traits;

#[allow(unused_imports)]
pub use schema::{
    apply_runtime_proxy_to_builder, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, runtime_proxy_config, set_runtime_proxy_config,
    AgentConfig, AgentsIpcConfig, AuditConfig, AutonomyConfig, BrowserComputerUseConfig,
    BrowserConfig, BuiltinHooksConfig, ChannelsConfig, ClassificationRule, ComposioConfig, Config,
    CoordinationConfig, CostConfig, CronConfig, DelegateAgentConfig, DiscordConfig,
    DockerRuntimeConfig, EmbeddingRouteConfig, EstopConfig, FeishuConfig, GatewayConfig,
    GroupReplyConfig, GroupReplyMode, HardwareConfig, HardwareTransport, HeartbeatConfig,
    HooksConfig, HttpRequestConfig, IMessageConfig, IdentityConfig, LarkConfig, MatrixConfig,
    MemoryConfig, ModelRouteConfig, MultimodalConfig, NextcloudTalkConfig,
    NonCliNaturalLanguageApprovalMode, ObservabilityConfig, OtpChallengeDelivery, OtpConfig,
    OtpMethod, PeripheralBoardConfig, PeripheralsConfig, ProviderConfig, ProxyConfig, ProxyScope,
    QdrantConfig, QueryClassificationConfig, ReliabilityConfig, ResearchPhaseConfig,
    ResearchTrigger, ResourceLimitsConfig, RuntimeConfig, SandboxBackend, SandboxConfig,
    SchedulerConfig, SecretsConfig, SecurityConfig, SecurityRoleConfig, SkillsConfig,
    SkillsPromptInjectionMode, SlackConfig, StorageConfig, StorageProviderConfig,
    StorageProviderSection, StreamMode, SyscallAnomalyConfig, TelegramConfig, TranscriptionConfig,
    TunnelConfig, WasmCapabilityEscalationMode, WasmModuleHashPolicy, WasmRuntimeConfig,
    WasmSecurityConfig, WebFetchConfig, WebSearchConfig, WebhookConfig,
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
            group_reply: None,
            base_url: None,
        };

        let discord = DiscordConfig {
            bot_token: "token".into(),
            guild_id: Some("123".into()),
            allowed_users: vec![],
            listen_to_bots: false,
            mention_only: false,
            group_reply: None,
        };

        let lark = LarkConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            mention_only: false,
            group_reply: None,
            use_feishu: false,
            receive_mode: crate::config::schema::LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        };
        let feishu = FeishuConfig {
            app_id: "app-id".into(),
            app_secret: "app-secret".into(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: vec![],
            group_reply: None,
            receive_mode: crate::config::schema::LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: crate::config::schema::default_lark_draft_update_interval_ms(
            ),
            max_draft_edits: crate::config::schema::default_lark_max_draft_edits(),
        };

        let nextcloud_talk = NextcloudTalkConfig {
            base_url: "https://cloud.example.com".into(),
            app_token: "app-token".into(),
            webhook_secret: None,
            allowed_users: vec!["*".into()],
        };

        assert_eq!(telegram.allowed_users.len(), 1);
        assert_eq!(discord.guild_id.as_deref(), Some("123"));
        assert_eq!(lark.app_id, "app-id");
        assert_eq!(feishu.app_id, "app-id");
        assert_eq!(nextcloud_talk.base_url, "https://cloud.example.com");
    }
}
