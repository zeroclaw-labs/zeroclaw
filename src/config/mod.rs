pub mod schema;
pub mod traits;

#[allow(unused_imports)]
pub use schema::{
    apply_runtime_proxy_to_builder, build_runtime_proxy_client,
    build_runtime_proxy_client_with_timeouts, default_model_fallback_for_provider,
    resolve_default_model_id, runtime_proxy_config, set_runtime_proxy_config,
    AckReactionChannelsConfig, AckReactionChatType, AckReactionConfig, AckReactionRuleAction,
    AckReactionRuleConfig, AckReactionStrategy, AgentConfig, AgentLoadBalanceStrategy,
    AgentSessionBackend, AgentSessionConfig, AgentSessionStrategy, AgentTeamsConfig,
    AgentsIpcConfig, AuditConfig, AutonomyConfig, BrowserComputerUseConfig, BrowserConfig,
    BuiltinHooksConfig, ChannelsConfig, ClassificationRule, CommandContextRuleAction,
    CommandContextRuleConfig, ComposioConfig, Config, CoordinationConfig, CostConfig, CronConfig,
    DelegateAgentConfig, DiscordConfig, DockerRuntimeConfig, EconomicConfig, EconomicTokenPricing,
    EmbeddingRouteConfig, EstopConfig, FeishuConfig, GatewayConfig, GroupReplyConfig,
    GroupReplyMode, HardwareConfig, HardwareTransport, HeartbeatConfig, HooksConfig,
    HttpRequestConfig, HttpRequestCredentialProfile, IMessageConfig, IdentityConfig, LarkConfig,
    MatrixConfig, MemoryConfig, ModelRouteConfig, MultimodalConfig, NextcloudTalkConfig,
    NonCliNaturalLanguageApprovalMode, ObservabilityConfig, OtpChallengeDelivery, OtpConfig,
    OtpMethod, OutboundLeakGuardAction, OutboundLeakGuardConfig, PeripheralBoardConfig,
    PeripheralsConfig, PerplexityFilterConfig, PluginEntryConfig, PluginsConfig, ProgressMode,
    ProviderConfig, ProxyConfig, ProxyScope, QdrantConfig, QueryClassificationConfig,
    ReliabilityConfig, ResearchPhaseConfig, ResearchTrigger, ResourceLimitsConfig, RuntimeConfig,
    SandboxBackend, SandboxConfig, SchedulerConfig, SecretsConfig, SecurityConfig,
    SecurityRoleConfig, SkillsConfig, SkillsPromptInjectionMode, SlackConfig, StorageConfig,
    StorageProviderConfig, StorageProviderSection, StreamMode, SubAgentsConfig,
    SyscallAnomalyConfig, TelegramConfig, TranscriptionConfig, TunnelConfig, UrlAccessConfig,
    WasmCapabilityEscalationMode, WasmConfig, WasmModuleHashPolicy, WasmRuntimeConfig,
    WasmSecurityConfig, WebFetchConfig, WebSearchConfig, WebhookConfig, DEFAULT_MODEL_FALLBACK,
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
            progress_mode: ProgressMode::default(),
            group_reply: None,
            base_url: None,
            ack_enabled: true,
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

    #[test]
    fn reexported_http_request_config_is_constructible() {
        let cfg = HttpRequestConfig {
            enabled: true,
            allowed_domains: vec!["api.openai.com".into()],
            max_response_size: 256_000,
            timeout_secs: 10,
            user_agent: "zeroclaw-test".into(),
            credential_profiles: std::collections::HashMap::new(),
        };

        assert!(cfg.enabled);
        assert_eq!(cfg.allowed_domains, vec!["api.openai.com"]);
        assert_eq!(cfg.max_response_size, 256_000);
        assert_eq!(cfg.timeout_secs, 10);
        assert_eq!(cfg.user_agent, "zeroclaw-test");
    }
}
