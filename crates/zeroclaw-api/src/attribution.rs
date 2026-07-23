use strum_macros::{EnumIter, EnumString, IntoStaticStr};

/// Trait every alias-bound "thing" implements once next to its struct.
pub trait Attributable {
    fn role(&self) -> Role;
    fn alias(&self) -> &str;
}

impl<T: Attributable + ?Sized> Attributable for std::sync::Arc<T> {
    fn role(&self) -> Role {
        (**self).role()
    }
    fn alias(&self) -> &str {
        (**self).alias()
    }
}

impl<T: Attributable + ?Sized> Attributable for Box<T> {
    fn role(&self) -> Role {
        (**self).role()
    }
    fn alias(&self) -> &str {
        (**self).alias()
    }
}

impl<T: Attributable + ?Sized> Attributable for &T {
    fn role(&self) -> Role {
        (**self).role()
    }
    fn alias(&self) -> &str {
        (**self).alias()
    }
}

/// Closed taxonomy of every role a thing can fill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Swarm,
    Agent,
    Channel(ChannelKind),
    Tool(ToolKind),
    Cron(CronKind),
    Provider(ProviderKind),
    Memory(MemoryKind),
    PeerGroup,
    Skill,
    Mcp,
    Sop,
    Session,
    System,
}

/// Channel implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr, EnumIter, EnumString)]
#[strum(serialize_all = "snake_case")]
pub enum ChannelKind {
    #[strum(serialize = "acp")]
    AcpChannel,
    #[strum(serialize = "amqp")]
    Amqp,
    Bluesky,
    #[strum(serialize = "clawdtalk")]
    ClawdTalk,
    Cli,
    #[strum(serialize = "dingtalk")]
    DingTalk,
    Discord,
    Email,
    Filesystem,
    Git,
    GmailPush,
    #[strum(serialize = "imessage")]
    IMessage,
    Irc,
    Lark,
    Line,
    Linq,
    Matrix,
    Mattermost,
    #[strum(serialize = "mochat")]
    MoChat,
    NextcloudTalk,
    Nostr,
    Notion,
    Qq,
    Reddit,
    Signal,
    Slack,
    Telegram,
    Twitch,
    Twitter,
    VoiceCall,
    VoiceWake,
    Wati,
    #[strum(serialize = "wecom")]
    WeCom,
    #[strum(serialize = "wecom_ws")]
    WeComWs,
    Webhook,
    Wechat,
    WhatsappBusiness,
    WhatsappWeb,
    Plugin,
}

impl ChannelKind {
    /// Whether this channel can deliver inbound events that fan into an SOP.
    /// `Cli` is a local interactive session, not a background event source, and
    /// `Plugin` is a synthetic attribution bucket; both are excluded. Everything
    /// else is a real inbound channel a SOP can trigger on.
    #[must_use]
    pub fn inbound_capable(self) -> bool {
        !matches!(self, Self::Cli | Self::Plugin)
    }

    /// Canonical snake_case wire string, single-sourced from `IntoStaticStr`.
    #[must_use]
    pub fn as_wire(self) -> &'static str {
        self.into()
    }
}

/// Serde adapter for `Option<ChannelKind>` that routes through the strum
/// string form, so the wire token is single-sourced from the enum's
/// `IntoStaticStr`/`EnumString` derives instead of a parallel serde map.
pub mod channel_kind_opt_serde {
    use super::ChannelKind;
    use core::str::FromStr;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<ChannelKind>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(kind) => serializer.serialize_some(kind.as_wire()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ChannelKind>, D::Error> {
        let opt = Option::<String>::deserialize(deserializer)?;
        match opt {
            Some(s) => ChannelKind::from_str(&s)
                .map(Some)
                .map_err(serde::de::Error::custom),
            None => Ok(None),
        }
    }
}

/// Built-in tool implementations. Closed set — plugins that need their
/// own attribution add a variant here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ToolKind {
    Shell,
    HttpRequest,
    HttpServer,
    FetchUrl,
    Search,
    Memory,
    SpawnSubagent,
    SopList,
    SopExecute,
    SopApprove,
    SopAdvance,
    SopStatus,
    SopHistory,
    Wait,
    Plugin,
}

/// Cron schedule shapes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum CronKind {
    Interval,
    At,
    Cron,
    Once,
}

/// Provider family. The inner enum carries the specific implementation;
/// the outer family drives which composite prefix (`model_provider` /
/// `tts_provider` / …) the layer populates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Model(ModelProviderKind),
    Tts(TtsProviderKind),
    Transcription(TranscriptionProviderKind),
    Tunnel(TunnelProviderKind),
}

impl ProviderKind {
    #[must_use]
    pub fn type_str(self) -> &'static str {
        match self {
            Self::Model(k) => k.into(),
            Self::Tts(k) => k.into(),
            Self::Transcription(k) => k.into(),
            Self::Tunnel(k) => k.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ModelProviderKind {
    Anthropic,
    #[strum(serialize = "openai")]
    OpenAi,
    #[strum(serialize = "openai_codex")]
    OpenAiCodex,
    Azure,
    Together,
    Bedrock,
    Ollama,
    Gemini,
    GeminiCli,
    GoogleAi,
    Mistral,
    Groq,
    OpenRouter,
    Telnyx,
    Copilot,
    Glm,
    KiloCli,
    Kilo,
    Router,
    Moonshot,
    Qwen,
    Minimax,
    Zai,
    Doubao,
    Yi,
    Hunyuan,
    Qianfan,
    Baichuan,
    Fireworks,
    Deepseek,
    AtomicChat,
    Cohere,
    Perplexity,
    Xai,
    Cerebras,
    Sambanova,
    Hyperbolic,
    Deepinfra,
    Huggingface,
    Ai21,
    Reka,
    Baseten,
    Nscale,
    Anyscale,
    Nebius,
    Friendli,
    Stepfun,
    Aihubmix,
    Siliconflow,
    Astrai,
    Avian,
    Deepmyst,
    Venice,
    Nearai,
    Novita,
    Nvidia,
    Vercel,
    Cloudflare,
    Ovh,
    Lmstudio,
    Llamacpp,
    Sglang,
    Vllm,
    Osaurus,
    Litellm,
    Lepton,
    Synthetic,
    Opencode,
    Custom,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum TtsProviderKind {
    #[strum(serialize = "openai")]
    OpenAi,
    #[strum(serialize = "elevenlabs")]
    ElevenLabs,
    Cartesia,
    Google,
    Edge,
    Piper,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum TranscriptionProviderKind {
    Whisper,
    #[strum(serialize = "openai")]
    OpenAi,
    Deepgram,
    Groq,
    AssemblyAi,
    Google,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum TunnelProviderKind {
    Ngrok,
    Cloudflared,
    OpenVpn,
    Pinggy,
    Tailscale,
    None,
    Custom,
    Plugin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum MemoryKind {
    Sqlite,
    Json,
    InMemory,
    Markdown,
    AgentScopedMarkdown,
    AgentScoped,
    Qdrant,
    Postgres,
    Lucid,
    None,
    Plugin,
}

impl Role {
    /// Composite prefix this role populates (`channel`, `model_provider`,
    /// `tts_provider`, `transcription_provider`, `tunnel_provider`),
    /// or `None` for roles that use a plain attribution field.
    #[must_use]
    pub fn composite_prefix(self) -> Option<&'static str> {
        match self {
            Self::Channel(_) => Some("channel"),
            Self::Provider(ProviderKind::Model(_)) => Some("model_provider"),
            Self::Provider(ProviderKind::Tts(_)) => Some("tts_provider"),
            Self::Provider(ProviderKind::Transcription(_)) => Some("transcription_provider"),
            Self::Provider(ProviderKind::Tunnel(_)) => Some("tunnel_provider"),
            _ => None,
        }
    }

    /// The `<type>` portion of the composite, when this role contributes
    /// to one.
    #[must_use]
    pub fn composite_type(self) -> Option<&'static str> {
        match self {
            Self::Channel(k) => Some(k.into()),
            Self::Provider(p) => Some(p.type_str()),
            _ => None,
        }
    }

    /// Plain-attribution-field key this role populates for roles that
    /// don't use a composite. `Tool` writes `tool`; `Agent` writes
    /// `agent_alias`; `Cron` writes `cron_job_id`; …
    #[must_use]
    pub fn attribution_field(self) -> Option<&'static str> {
        match self {
            Self::Agent => Some("agent_alias"),
            Self::Tool(_) => Some("tool"),
            Self::Cron(_) => Some("cron_job_id"),
            Self::Memory(_) => Some("memory_namespace"),
            Self::PeerGroup => Some("peer_group"),
            Self::Skill => Some("skill_bundle"),
            Self::Mcp => Some("mcp_bundle"),
            Self::Sop => Some("sop_name"),
            Self::Session => Some("session_key"),
            Self::System => Some("system_alias"),
            _ => None,
        }
    }

    /// Stable string tag used by the span layer to identify the role's
    /// family. The inner Kind (when applicable) is rendered alongside in
    /// [`Role::composite_type`].
    #[must_use]
    pub fn family_str(self) -> &'static str {
        match self {
            Self::Swarm => "swarm",
            Self::Agent => "agent",
            Self::Channel(_) => "channel",
            Self::Tool(_) => "tool",
            Self::Cron(_) => "cron",
            Self::Provider(ProviderKind::Model(_)) => "provider.model",
            Self::Provider(ProviderKind::Tts(_)) => "provider.tts",
            Self::Provider(ProviderKind::Transcription(_)) => "provider.transcription",
            Self::Provider(ProviderKind::Tunnel(_)) => "provider.tunnel",
            Self::Memory(_) => "memory",
            Self::PeerGroup => "peer_group",
            Self::Skill => "skill",
            Self::Mcp => "mcp",
            Self::Sop => "sop",
            Self::Session => "session",
            Self::System => "system",
        }
    }

    /// Closest `zeroclaw_log::EventCategory` for this role, used by
    /// the layer to default `event.category` when the call site doesn't
    /// override. Returned as a `&'static str` to keep `zeroclaw-api`
    /// free of a back-dep on `zeroclaw-log`.
    #[must_use]
    pub fn default_category(self) -> &'static str {
        match self {
            Self::Swarm | Self::Agent => "agent",
            Self::Channel(_) => "channel",
            Self::Tool(_) => "tool",
            Self::Cron(_) => "cron",
            Self::Provider(_) => "provider",
            Self::Memory(_) => "memory",
            Self::Session => "session",
            Self::Sop => "system",
            Self::PeerGroup | Self::Skill | Self::Mcp | Self::System => "system",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_kind_snake_case() {
        assert_eq!(<&'static str>::from(ChannelKind::Telegram), "telegram");
        assert_eq!(
            <&'static str>::from(ChannelKind::WhatsappBusiness),
            "whatsapp_business"
        );
    }

    #[test]
    fn provider_kind_delegates_to_inner() {
        assert_eq!(
            ProviderKind::Model(ModelProviderKind::Anthropic).type_str(),
            "anthropic"
        );
        assert_eq!(
            ProviderKind::Tts(TtsProviderKind::ElevenLabs).type_str(),
            "elevenlabs"
        );
    }

    #[test]
    fn role_composite_prefix() {
        assert_eq!(
            Role::Channel(ChannelKind::Discord).composite_prefix(),
            Some("channel")
        );
        assert_eq!(
            Role::Provider(ProviderKind::Model(ModelProviderKind::Anthropic)).composite_prefix(),
            Some("model_provider"),
        );
        assert!(Role::Agent.composite_prefix().is_none());
    }

    #[test]
    fn role_attribution_field() {
        assert_eq!(Role::Agent.attribution_field(), Some("agent_alias"));
        assert_eq!(
            Role::Tool(ToolKind::Shell).attribution_field(),
            Some("tool")
        );
        assert!(
            Role::Channel(ChannelKind::Telegram)
                .attribution_field()
                .is_none()
        );
        assert_eq!(Role::System.attribution_field(), Some("system_alias"));
    }

    #[test]
    fn role_family_str_returns_stable_tags() {
        assert_eq!(Role::Agent.family_str(), "agent");
        assert_eq!(Role::Swarm.family_str(), "swarm");
        assert_eq!(Role::Channel(ChannelKind::Discord).family_str(), "channel");
        assert_eq!(Role::Tool(ToolKind::Shell).family_str(), "tool");
        assert_eq!(Role::Cron(CronKind::Interval).family_str(), "cron");
        assert_eq!(Role::Memory(MemoryKind::Sqlite).family_str(), "memory");
        assert_eq!(Role::Session.family_str(), "session");
        assert_eq!(Role::System.family_str(), "system");
    }
}
