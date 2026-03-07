//! Lightweight web-based onboard wizard server.
//!
//! Launches a standalone HTTP server (default `127.0.0.1`) that serves the embedded
//! web dashboard and exposes structured JSON APIs for form-based configuration.
//! Unlike the full gateway, this server requires no pairing, no channels, and
//! carries minimal state — just the config being edited.
//!
//! **Security:** When binding to a non-localhost address, a one-time pairing code
//! is generated and printed in the terminal. The frontend must POST /pair with the
//! code to obtain a bearer token before accessing API endpoints.
//!
//! DTO sections mirror the interactive wizard's 9 steps:
//!   1. Workspace  2. Provider  3. Channels  4. Tunnel
//!   5. Tool Mode & Security  6. Hardware  7. Memory
//!   8. Project Context  9. Autonomy & Gateway

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tracing::info;

use crate::config::Config;
use crate::providers::list_providers;
use crate::security::PairingGuard;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct OnboardState {
    config: Arc<Mutex<Config>>,
    pairing: Arc<PairingGuard>,
    project_context: Arc<Mutex<ProjectContextSetup>>,
}

// ---------------------------------------------------------------------------
// DTOs — structured JSON for form binding (maps to wizard steps 1–9)
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
pub struct SetupConfig {
    /// Step 1: Workspace
    pub workspace: WorkspaceSetup,
    /// Step 2: AI Provider & API Key
    pub provider: ProviderSetup,
    /// Step 3: Channels
    pub channels: ChannelsSetup,
    /// Step 4: Tunnel
    pub tunnel: TunnelSetup,
    /// Step 5: Tool Mode & Security
    pub tool_mode: ToolModeSetup,
    /// Step 6: Hardware
    pub hardware: HardwareSetup,
    /// Step 7: Memory
    pub memory: MemorySetup,
    /// Step 8: Project Context (Personalize Your Agent)
    pub project_context: ProjectContextSetup,
    /// Step 9: Autonomy & Gateway
    pub autonomy: AutonomySetup,
    pub gateway: GatewaySetup,
}

// -- Step 1 --

#[derive(Debug, Serialize, Deserialize)]
pub struct WorkspaceSetup {
    pub path: String,
}

// -- Step 2 --

#[derive(Debug, Serialize, Deserialize)]
pub struct ProviderSetup {
    pub name: String,
    pub api_key: String,
    pub model: String,
    pub api_url: String,
}

// -- Step 3: Channels --

#[derive(Debug, Serialize, Deserialize)]
pub struct ChannelsSetup {
    pub telegram: TelegramSetup,
    pub discord: DiscordSetup,
    pub slack: SlackSetup,
    pub mattermost: MattermostSetup,
    pub webhook: WebhookSetup,
    pub imessage: IMessageSetup,
    pub matrix: MatrixSetup,
    pub signal: SignalSetup,
    pub whatsapp: WhatsAppSetup,
    pub linq: LinqSetup,
    pub wati: WatiSetup,
    pub nextcloud_talk: NextcloudTalkSetup,
    pub irc: IrcSetup,
    pub lark: LarkSetup,
    pub feishu: FeishuSetup,
    pub dingtalk: DingTalkSetup,
    pub qq: QQSetup,
    pub nostr: NostrSetup,
    pub email: EmailSetup,
    pub clawdtalk: ClawdTalkSetup,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TelegramSetup {
    pub enabled: bool,
    pub bot_token: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    /// "off" or "partial"
    pub stream_mode: String,
    pub draft_update_interval_ms: u64,
    pub interrupt_on_new_message: bool,
    pub mention_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DiscordSetup {
    pub enabled: bool,
    pub bot_token: String,
    pub guild_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    pub listen_to_bots: bool,
    pub mention_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SlackSetup {
    pub enabled: bool,
    pub bot_token: String,
    pub app_token: String,
    pub channel_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MattermostSetup {
    pub enabled: bool,
    pub url: String,
    pub bot_token: String,
    pub channel_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    pub thread_replies: bool,
    pub mention_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookSetup {
    pub enabled: bool,
    pub port: u16,
    pub secret: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IMessageSetup {
    pub enabled: bool,
    /// Comma-separated list or `*` for all
    pub allowed_contacts: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MatrixSetup {
    pub enabled: bool,
    pub homeserver: String,
    pub access_token: String,
    pub user_id: String,
    pub device_id: String,
    pub room_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    pub mention_only: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SignalSetup {
    pub enabled: bool,
    pub http_url: String,
    pub account: String,
    pub group_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_from: String,
    pub ignore_attachments: bool,
    pub ignore_stories: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WhatsAppSetup {
    pub enabled: bool,
    // Business API fields
    pub access_token: String,
    pub phone_number_id: String,
    pub verify_token: String,
    pub app_secret: String,
    // Web/pairing fields
    pub session_path: String,
    pub pair_phone: String,
    pub pair_code: String,
    /// Comma-separated list or `*` for all
    pub allowed_numbers: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LinqSetup {
    pub enabled: bool,
    pub api_token: String,
    pub from_phone: String,
    pub signing_secret: String,
    /// Comma-separated list or `*` for all
    pub allowed_senders: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WatiSetup {
    pub enabled: bool,
    pub api_token: String,
    pub api_url: String,
    pub tenant_id: String,
    /// Comma-separated list or `*` for all
    pub allowed_numbers: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NextcloudTalkSetup {
    pub enabled: bool,
    pub base_url: String,
    pub app_token: String,
    pub webhook_secret: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct IrcSetup {
    pub enabled: bool,
    pub server: String,
    pub port: u16,
    pub nickname: String,
    pub username: String,
    /// Comma-separated list of channels
    pub channels: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    pub server_password: String,
    pub nickserv_password: String,
    pub sasl_password: String,
    pub verify_tls: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LarkSetup {
    pub enabled: bool,
    pub app_id: String,
    pub app_secret: String,
    pub encrypt_key: String,
    pub verification_token: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    pub mention_only: bool,
    pub use_feishu: bool,
    /// "websocket" or "webhook"
    pub receive_mode: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FeishuSetup {
    pub enabled: bool,
    pub app_id: String,
    pub app_secret: String,
    pub encrypt_key: String,
    pub verification_token: String,
    /// Comma-separated list or `*` for all
    pub allowed_users: String,
    /// "websocket" or "webhook"
    pub receive_mode: String,
    pub port: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DingTalkSetup {
    pub enabled: bool,
    pub client_id: String,
    pub client_secret: String,
    /// Comma-separated list
    pub allowed_users: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct QQSetup {
    pub enabled: bool,
    pub app_id: String,
    pub app_secret: String,
    /// Comma-separated list
    pub allowed_users: String,
    /// "webhook" or "websocket"
    pub receive_mode: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NostrSetup {
    pub enabled: bool,
    pub private_key: String,
    /// Comma-separated list of relay URLs
    pub relays: String,
    /// Comma-separated list or `*` for all
    pub allowed_pubkeys: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EmailSetup {
    pub enabled: bool,
    pub imap_host: String,
    pub imap_port: u16,
    pub imap_folder: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_tls: bool,
    pub username: String,
    pub password: String,
    pub from_address: String,
    pub idle_timeout_secs: u64,
    /// Comma-separated list or `*` for all
    pub allowed_senders: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClawdTalkSetup {
    pub enabled: bool,
    pub api_key: String,
    pub connection_id: String,
    pub from_number: String,
    /// Comma-separated list
    pub allowed_destinations: String,
    pub webhook_secret: String,
}

// -- Step 4 --

#[derive(Debug, Serialize, Deserialize)]
pub struct TunnelSetup {
    /// "none", "cloudflare", "ngrok", "tailscale", "custom"
    pub provider: String,
    pub cloudflare_token: String,
    pub ngrok_auth_token: String,
    pub ngrok_domain: String,
    pub tailscale_funnel: bool,
    pub tailscale_hostname: String,
    pub custom_start_command: String,
}

// -- Step 5 --

#[derive(Debug, Serialize, Deserialize)]
pub struct ToolModeSetup {
    pub composio_enabled: bool,
    pub composio_api_key: String,
    pub secrets_encrypt: bool,
}

// -- Step 6 --

#[derive(Debug, Serialize, Deserialize)]
pub struct HardwareSetup {
    pub enabled: bool,
    /// "none", "native", "serial", "probe"
    pub transport: String,
    pub serial_port: String,
    pub baud_rate: u32,
    pub probe_target: String,
    pub workspace_datasheets: bool,
}

// -- Step 7 --

#[derive(Debug, Serialize, Deserialize)]
pub struct MemorySetup {
    pub backend: String,
    pub auto_save: bool,
    // Lucid/SQLite hygiene fields (only relevant for sqlite-based backends)
    pub hygiene_enabled: bool,
    pub archive_after_days: u32,
    pub purge_after_days: u32,
    pub embedding_cache_size: usize,
}

// -- Step 8 --

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectContextSetup {
    pub user_name: String,
    pub timezone: String,
    pub agent_name: String,
    pub communication_style: String,
}

impl Default for ProjectContextSetup {
    fn default() -> Self {
        Self {
            user_name: "User".to_string(),
            timezone: "UTC".to_string(),
            agent_name: "ZeroClaw".to_string(),
            communication_style: "Friendly & casual".to_string(),
        }
    }
}

// -- Step 9 --

#[derive(Debug, Serialize, Deserialize)]
pub struct AutonomySetup {
    pub level: String,
    pub max_actions_per_hour: u32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct GatewaySetup {
    pub host: String,
    pub port: u16,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

const MASKED_KEY: &str = "********";

fn mask_optional(val: Option<&String>) -> String {
    match val {
        Some(k) if !k.is_empty() => MASKED_KEY.to_string(),
        _ => String::new(),
    }
}

fn mask_str(val: &str) -> String {
    if val.is_empty() {
        String::new()
    } else {
        MASKED_KEY.to_string()
    }
}

fn join_list(v: &[String]) -> String {
    v.join(", ")
}

fn split_list(s: &str) -> Vec<String> {
    if s.trim().is_empty() {
        Vec::new()
    } else {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

/// Returns the value if it's non-empty and not the mask sentinel.
fn unmask(val: &str) -> Option<String> {
    if val.is_empty() || val == MASKED_KEY {
        None
    } else {
        Some(val.to_string())
    }
}

// ---------------------------------------------------------------------------
// Config -> DTO
// ---------------------------------------------------------------------------

fn config_to_setup(config: &Config) -> SetupConfig {
    SetupConfig {
        // Step 1: Workspace
        workspace: WorkspaceSetup {
            path: config.workspace_dir.display().to_string(),
        },
        // Step 2: Provider
        provider: ProviderSetup {
            name: config
                .default_provider
                .clone()
                .unwrap_or_else(|| "openrouter".to_string()),
            api_key: mask_optional(config.api_key.as_ref()),
            model: config.default_model.clone().unwrap_or_default(),
            api_url: config.api_url.clone().unwrap_or_default(),
        },
        // Step 3: Channels
        channels: channels_to_setup(&config.channels_config),
        // Step 4: Tunnel
        tunnel: TunnelSetup {
            provider: config.tunnel.provider.clone(),
            cloudflare_token: config
                .tunnel
                .cloudflare
                .as_ref()
                .map(|_| MASKED_KEY.to_string())
                .unwrap_or_default(),
            ngrok_auth_token: config
                .tunnel
                .ngrok
                .as_ref()
                .map(|_| MASKED_KEY.to_string())
                .unwrap_or_default(),
            ngrok_domain: config
                .tunnel
                .ngrok
                .as_ref()
                .and_then(|n| n.domain.clone())
                .unwrap_or_default(),
            tailscale_funnel: config
                .tunnel
                .tailscale
                .as_ref()
                .map(|t| t.funnel)
                .unwrap_or(false),
            tailscale_hostname: config
                .tunnel
                .tailscale
                .as_ref()
                .and_then(|t| t.hostname.clone())
                .unwrap_or_default(),
            custom_start_command: config
                .tunnel
                .custom
                .as_ref()
                .map(|c| c.start_command.clone())
                .unwrap_or_default(),
        },
        // Step 5: Tool Mode & Security
        tool_mode: ToolModeSetup {
            composio_enabled: config.composio.enabled,
            composio_api_key: mask_optional(config.composio.api_key.as_ref()),
            secrets_encrypt: config.secrets.encrypt,
        },
        // Step 6: Hardware
        hardware: HardwareSetup {
            enabled: config.hardware.enabled,
            transport: config.hardware.transport.to_string(),
            serial_port: config.hardware.serial_port.clone().unwrap_or_default(),
            baud_rate: config.hardware.baud_rate,
            probe_target: config.hardware.probe_target.clone().unwrap_or_default(),
            workspace_datasheets: config.hardware.workspace_datasheets,
        },
        // Step 7: Memory
        memory: MemorySetup {
            backend: config.memory.backend.clone(),
            auto_save: config.memory.auto_save,
            hygiene_enabled: config.memory.hygiene_enabled,
            archive_after_days: config.memory.archive_after_days,
            purge_after_days: config.memory.purge_after_days,
            embedding_cache_size: config.memory.embedding_cache_size,
        },
        // Step 8: Project Context (not stored in config, returns defaults)
        project_context: ProjectContextSetup::default(),
        // Step 9: Autonomy & Gateway
        autonomy: AutonomySetup {
            level: format!("{:?}", config.autonomy.level).to_lowercase(),
            max_actions_per_hour: config.autonomy.max_actions_per_hour,
        },
        gateway: GatewaySetup {
            host: config.gateway.host.clone(),
            port: config.gateway.port,
        },
    }
}

fn channels_to_setup(ch: &crate::config::schema::ChannelsConfig) -> ChannelsSetup {
    ChannelsSetup {
        telegram: {
            let c = ch.telegram.as_ref();
            TelegramSetup {
                enabled: c.is_some(),
                bot_token: c
                    .filter(|t| !t.bot_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|t| join_list(&t.allowed_users)).unwrap_or_default(),
                stream_mode: c
                    .map(|t| format!("{:?}", t.stream_mode).to_lowercase())
                    .unwrap_or_else(|| "off".to_string()),
                draft_update_interval_ms: c.map(|t| t.draft_update_interval_ms).unwrap_or(500),
                interrupt_on_new_message: c.map(|t| t.interrupt_on_new_message).unwrap_or(false),
                mention_only: c.map(|t| t.mention_only).unwrap_or(false),
            }
        },
        discord: {
            let c = ch.discord.as_ref();
            DiscordSetup {
                enabled: c.is_some(),
                bot_token: c
                    .filter(|d| !d.bot_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                guild_id: c.and_then(|d| d.guild_id.clone()).unwrap_or_default(),
                allowed_users: c.map(|d| join_list(&d.allowed_users)).unwrap_or_default(),
                listen_to_bots: c.map(|d| d.listen_to_bots).unwrap_or(false),
                mention_only: c.map(|d| d.mention_only).unwrap_or(false),
            }
        },
        slack: {
            let c = ch.slack.as_ref();
            SlackSetup {
                enabled: c.is_some(),
                bot_token: c
                    .filter(|s| !s.bot_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                app_token: c
                    .and_then(|s| s.app_token.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                channel_id: c.and_then(|s| s.channel_id.clone()).unwrap_or_default(),
                allowed_users: c.map(|s| join_list(&s.allowed_users)).unwrap_or_default(),
            }
        },
        mattermost: {
            let c = ch.mattermost.as_ref();
            MattermostSetup {
                enabled: c.is_some(),
                url: c.map(|m| m.url.clone()).unwrap_or_default(),
                bot_token: c
                    .filter(|m| !m.bot_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                channel_id: c.and_then(|m| m.channel_id.clone()).unwrap_or_default(),
                allowed_users: c.map(|m| join_list(&m.allowed_users)).unwrap_or_default(),
                thread_replies: c.and_then(|m| m.thread_replies).unwrap_or(false),
                mention_only: c.and_then(|m| m.mention_only).unwrap_or(false),
            }
        },
        webhook: {
            let c = ch.webhook.as_ref();
            WebhookSetup {
                enabled: c.is_some(),
                port: c.map(|w| w.port).unwrap_or(8080),
                secret: c
                    .and_then(|w| w.secret.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
            }
        },
        imessage: {
            let c = ch.imessage.as_ref();
            IMessageSetup {
                enabled: c.is_some(),
                allowed_contacts: c
                    .map(|i| join_list(&i.allowed_contacts))
                    .unwrap_or_default(),
            }
        },
        matrix: {
            let c = ch.matrix.as_ref();
            MatrixSetup {
                enabled: c.is_some(),
                homeserver: c.map(|m| m.homeserver.clone()).unwrap_or_default(),
                access_token: c
                    .filter(|m| !m.access_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                user_id: c.and_then(|m| m.user_id.clone()).unwrap_or_default(),
                device_id: c.and_then(|m| m.device_id.clone()).unwrap_or_default(),
                room_id: c.map(|m| m.room_id.clone()).unwrap_or_default(),
                allowed_users: c.map(|m| join_list(&m.allowed_users)).unwrap_or_default(),
                mention_only: c.is_some_and(|m| m.mention_only),
            }
        },
        signal: {
            let c = ch.signal.as_ref();
            SignalSetup {
                enabled: c.is_some(),
                http_url: c
                    .map(|s| s.http_url.clone())
                    .unwrap_or_else(|| "http://127.0.0.1:8686".to_string()),
                account: c.map(|s| s.account.clone()).unwrap_or_default(),
                group_id: c.and_then(|s| s.group_id.clone()).unwrap_or_default(),
                allowed_from: c.map(|s| join_list(&s.allowed_from)).unwrap_or_default(),
                ignore_attachments: c.map(|s| s.ignore_attachments).unwrap_or(false),
                ignore_stories: c.map(|s| s.ignore_stories).unwrap_or(true),
            }
        },
        whatsapp: {
            let c = ch.whatsapp.as_ref();
            WhatsAppSetup {
                enabled: c.is_some(),
                access_token: c
                    .and_then(|w| w.access_token.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                phone_number_id: c
                    .and_then(|w| w.phone_number_id.clone())
                    .unwrap_or_default(),
                verify_token: c
                    .and_then(|w| w.verify_token.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                app_secret: c
                    .and_then(|w| w.app_secret.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                session_path: c.and_then(|w| w.session_path.clone()).unwrap_or_default(),
                pair_phone: c.and_then(|w| w.pair_phone.clone()).unwrap_or_default(),
                pair_code: c
                    .and_then(|w| w.pair_code.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_numbers: c.map(|w| join_list(&w.allowed_numbers)).unwrap_or_default(),
            }
        },
        linq: {
            let c = ch.linq.as_ref();
            LinqSetup {
                enabled: c.is_some(),
                api_token: c
                    .filter(|l| !l.api_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                from_phone: c.map(|l| l.from_phone.clone()).unwrap_or_default(),
                signing_secret: c
                    .and_then(|l| l.signing_secret.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_senders: c.map(|l| join_list(&l.allowed_senders)).unwrap_or_default(),
            }
        },
        wati: {
            let c = ch.wati.as_ref();
            WatiSetup {
                enabled: c.is_some(),
                api_token: c
                    .filter(|w| !w.api_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                api_url: c.map(|w| w.api_url.clone()).unwrap_or_default(),
                tenant_id: c.and_then(|w| w.tenant_id.clone()).unwrap_or_default(),
                allowed_numbers: c.map(|w| join_list(&w.allowed_numbers)).unwrap_or_default(),
            }
        },
        nextcloud_talk: {
            let c = ch.nextcloud_talk.as_ref();
            NextcloudTalkSetup {
                enabled: c.is_some(),
                base_url: c.map(|n| n.base_url.clone()).unwrap_or_default(),
                app_token: c
                    .filter(|n| !n.app_token.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                webhook_secret: c
                    .and_then(|n| n.webhook_secret.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|n| join_list(&n.allowed_users)).unwrap_or_default(),
            }
        },
        irc: {
            let c = ch.irc.as_ref();
            IrcSetup {
                enabled: c.is_some(),
                server: c.map(|i| i.server.clone()).unwrap_or_default(),
                port: c.map(|i| i.port).unwrap_or(6697),
                nickname: c.map(|i| i.nickname.clone()).unwrap_or_default(),
                username: c.and_then(|i| i.username.clone()).unwrap_or_default(),
                channels: c.map(|i| join_list(&i.channels)).unwrap_or_default(),
                allowed_users: c.map(|i| join_list(&i.allowed_users)).unwrap_or_default(),
                server_password: c
                    .and_then(|i| i.server_password.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                nickserv_password: c
                    .and_then(|i| i.nickserv_password.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                sasl_password: c
                    .and_then(|i| i.sasl_password.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                verify_tls: c.and_then(|i| i.verify_tls).unwrap_or(true),
            }
        },
        lark: {
            let c = ch.lark.as_ref();
            LarkSetup {
                enabled: c.is_some(),
                app_id: c.map(|l| l.app_id.clone()).unwrap_or_default(),
                app_secret: c
                    .filter(|l| !l.app_secret.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                encrypt_key: c
                    .and_then(|l| l.encrypt_key.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                verification_token: c
                    .and_then(|l| l.verification_token.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|l| join_list(&l.allowed_users)).unwrap_or_default(),
                mention_only: c.map(|l| l.mention_only).unwrap_or(false),
                use_feishu: c.map(|l| l.use_feishu).unwrap_or(false),
                receive_mode: c
                    .map(|l| format!("{:?}", l.receive_mode).to_lowercase())
                    .unwrap_or_else(|| "websocket".to_string()),
                port: c.and_then(|l| l.port).unwrap_or(9000),
            }
        },
        feishu: {
            let c = ch.feishu.as_ref();
            FeishuSetup {
                enabled: c.is_some(),
                app_id: c.map(|f| f.app_id.clone()).unwrap_or_default(),
                app_secret: c
                    .filter(|f| !f.app_secret.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                encrypt_key: c
                    .and_then(|f| f.encrypt_key.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                verification_token: c
                    .and_then(|f| f.verification_token.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|f| join_list(&f.allowed_users)).unwrap_or_default(),
                receive_mode: c
                    .map(|f| format!("{:?}", f.receive_mode).to_lowercase())
                    .unwrap_or_else(|| "websocket".to_string()),
                port: c.and_then(|f| f.port).unwrap_or(9000),
            }
        },
        dingtalk: {
            let c = ch.dingtalk.as_ref();
            DingTalkSetup {
                enabled: c.is_some(),
                client_id: c.map(|d| d.client_id.clone()).unwrap_or_default(),
                client_secret: c
                    .filter(|d| !d.client_secret.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|d| join_list(&d.allowed_users)).unwrap_or_default(),
            }
        },
        qq: {
            let c = ch.qq.as_ref();
            QQSetup {
                enabled: c.is_some(),
                app_id: c.map(|q| q.app_id.clone()).unwrap_or_default(),
                app_secret: c
                    .filter(|q| !q.app_secret.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                allowed_users: c.map(|q| join_list(&q.allowed_users)).unwrap_or_default(),
                receive_mode: c
                    .map(|q| match q.receive_mode {
                        crate::config::schema::QQReceiveMode::Websocket => "websocket",
                        crate::config::schema::QQReceiveMode::Webhook => "webhook",
                    })
                    .unwrap_or("webhook")
                    .to_string(),
            }
        },
        nostr: {
            let c = ch.nostr.as_ref();
            NostrSetup {
                enabled: c.is_some(),
                private_key: c
                    .filter(|n| !n.private_key.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                relays: c.map(|n| join_list(&n.relays)).unwrap_or_default(),
                allowed_pubkeys: c.map(|n| join_list(&n.allowed_pubkeys)).unwrap_or_default(),
            }
        },
        email: {
            let c = ch.email.as_ref();
            EmailSetup {
                enabled: c.is_some(),
                imap_host: c.map(|e| e.imap_host.clone()).unwrap_or_default(),
                imap_port: c.map(|e| e.imap_port).unwrap_or(993),
                imap_folder: c
                    .map(|e| e.imap_folder.clone())
                    .unwrap_or_else(|| "INBOX".to_string()),
                smtp_host: c.map(|e| e.smtp_host.clone()).unwrap_or_default(),
                smtp_port: c.map(|e| e.smtp_port).unwrap_or(587),
                smtp_tls: c.map(|e| e.smtp_tls).unwrap_or(true),
                username: c.map(|e| e.username.clone()).unwrap_or_default(),
                password: c
                    .filter(|e| !e.password.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                from_address: c.map(|e| e.from_address.clone()).unwrap_or_default(),
                idle_timeout_secs: c.map(|e| e.idle_timeout_secs).unwrap_or(300),
                allowed_senders: c.map(|e| join_list(&e.allowed_senders)).unwrap_or_default(),
            }
        },
        clawdtalk: {
            let c = ch.clawdtalk.as_ref();
            ClawdTalkSetup {
                enabled: c.is_some(),
                api_key: c
                    .filter(|ct| !ct.api_key.is_empty())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
                connection_id: c.map(|ct| ct.connection_id.clone()).unwrap_or_default(),
                from_number: c.map(|ct| ct.from_number.clone()).unwrap_or_default(),
                allowed_destinations: c
                    .map(|ct| join_list(&ct.allowed_destinations))
                    .unwrap_or_default(),
                webhook_secret: c
                    .and_then(|ct| ct.webhook_secret.as_ref())
                    .map(|_| MASKED_KEY.to_string())
                    .unwrap_or_default(),
            }
        },
    }
}

// ---------------------------------------------------------------------------
// DTO -> Config
// ---------------------------------------------------------------------------

fn apply_setup_to_config(setup: &SetupConfig, config: &mut Config) {
    use crate::config::schema::{
        CloudflareTunnelConfig, CustomTunnelConfig, NgrokTunnelConfig, TailscaleTunnelConfig,
    };

    // Step 1: Workspace
    if !setup.workspace.path.is_empty() {
        config.workspace_dir = std::path::PathBuf::from(&setup.workspace.path);
    }

    // Step 2: Provider
    config.default_provider = Some(setup.provider.name.clone());
    if let Some(key) = unmask(&setup.provider.api_key) {
        config.api_key = Some(key);
    }
    config.default_model = if setup.provider.model.is_empty() {
        None
    } else {
        Some(setup.provider.model.clone())
    };
    config.api_url = if setup.provider.api_url.is_empty() {
        None
    } else {
        Some(setup.provider.api_url.clone())
    };

    // Step 3: Channels
    apply_channels(&setup.channels, &mut config.channels_config);

    // Step 4: Tunnel
    config.tunnel.provider = setup.tunnel.provider.clone();
    match setup.tunnel.provider.as_str() {
        "cloudflare" => {
            let existing_token = config
                .tunnel
                .cloudflare
                .as_ref()
                .map(|c| c.token.clone())
                .unwrap_or_default();
            let token = unmask(&setup.tunnel.cloudflare_token).unwrap_or(existing_token);
            config.tunnel.cloudflare = Some(CloudflareTunnelConfig { token });
        }
        "ngrok" => {
            let existing = config.tunnel.ngrok.take();
            let auth_token = unmask(&setup.tunnel.ngrok_auth_token).unwrap_or_else(|| {
                existing
                    .as_ref()
                    .map(|n| n.auth_token.clone())
                    .unwrap_or_default()
            });
            let domain = if setup.tunnel.ngrok_domain.is_empty() {
                None
            } else {
                Some(setup.tunnel.ngrok_domain.clone())
            };
            config.tunnel.ngrok = Some(NgrokTunnelConfig { auth_token, domain });
        }
        "tailscale" => {
            let hostname = if setup.tunnel.tailscale_hostname.is_empty() {
                None
            } else {
                Some(setup.tunnel.tailscale_hostname.clone())
            };
            config.tunnel.tailscale = Some(TailscaleTunnelConfig {
                funnel: setup.tunnel.tailscale_funnel,
                hostname,
            });
        }
        "custom" => {
            if !setup.tunnel.custom_start_command.is_empty() {
                config.tunnel.custom = Some(CustomTunnelConfig {
                    start_command: setup.tunnel.custom_start_command.clone(),
                    health_url: None,
                    url_pattern: None,
                });
            }
        }
        _ => {
            // "none" — clear sub-configs
        }
    }

    // Step 5: Tool Mode & Security
    config.composio.enabled = setup.tool_mode.composio_enabled;
    if let Some(key) = unmask(&setup.tool_mode.composio_api_key) {
        config.composio.api_key = Some(key);
    }
    config.secrets.encrypt = setup.tool_mode.secrets_encrypt;

    // Step 6: Hardware
    config.hardware.enabled = setup.hardware.enabled;
    config.hardware.transport = match setup.hardware.transport.as_str() {
        "native" => crate::config::schema::HardwareTransport::Native,
        "serial" => crate::config::schema::HardwareTransport::Serial,
        "probe" => crate::config::schema::HardwareTransport::Probe,
        _ => crate::config::schema::HardwareTransport::None,
    };
    config.hardware.serial_port = if setup.hardware.serial_port.is_empty() {
        None
    } else {
        Some(setup.hardware.serial_port.clone())
    };
    config.hardware.baud_rate = setup.hardware.baud_rate;
    config.hardware.probe_target = if setup.hardware.probe_target.is_empty() {
        None
    } else {
        Some(setup.hardware.probe_target.clone())
    };
    config.hardware.workspace_datasheets = setup.hardware.workspace_datasheets;

    // Step 7: Memory
    config.memory.backend = setup.memory.backend.clone();
    config.memory.auto_save = setup.memory.auto_save;
    config.memory.hygiene_enabled = setup.memory.hygiene_enabled;
    config.memory.archive_after_days = setup.memory.archive_after_days;
    config.memory.purge_after_days = setup.memory.purge_after_days;
    config.memory.embedding_cache_size = setup.memory.embedding_cache_size;

    // Step 8: Project Context — not stored in config (used for workspace file scaffolding)
    // Handled separately in the PUT handler.

    // Step 9: Autonomy & Gateway
    if let Ok(level) = setup.autonomy.level.parse() {
        config.autonomy.level = level;
    }
    config.autonomy.max_actions_per_hour = setup.autonomy.max_actions_per_hour;
    config.gateway.host = setup.gateway.host.clone();
    config.gateway.port = setup.gateway.port;
}

/// Apply all 20 channel settings from the DTO to the config.
fn apply_channels(ch: &ChannelsSetup, cfg: &mut crate::config::schema::ChannelsConfig) {
    use crate::channels::clawdtalk::ClawdTalkConfig;
    use crate::channels::email_channel::{EmailConfig, EmailImapIdConfig};
    use crate::config::schema::{
        DingTalkConfig, DiscordConfig, FeishuConfig, IMessageConfig, IrcConfig, LarkConfig,
        LarkReceiveMode, LinqConfig, MatrixConfig, MattermostConfig, NextcloudTalkConfig,
        NostrConfig, ProgressMode, QQConfig, QQEnvironment, QQReceiveMode, SignalConfig,
        SlackConfig, StreamMode, TelegramConfig, WatiConfig, WebhookConfig, WhatsAppConfig,
    };

    // -- Telegram --
    if ch.telegram.enabled {
        let mut ex = cfg.telegram.take().unwrap_or_else(|| TelegramConfig {
            bot_token: String::new(),
            allowed_users: Vec::new(),
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 500,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_enabled: true,
            base_url: None,
            group_reply: None,
            progress_mode: ProgressMode::default(),
        });
        if let Some(v) = unmask(&ch.telegram.bot_token) {
            ex.bot_token = v;
        }
        ex.allowed_users = split_list(&ch.telegram.allowed_users);
        ex.stream_mode = match ch.telegram.stream_mode.as_str() {
            "partial" => StreamMode::Partial,
            _ => StreamMode::Off,
        };
        ex.draft_update_interval_ms = ch.telegram.draft_update_interval_ms;
        ex.interrupt_on_new_message = ch.telegram.interrupt_on_new_message;
        ex.mention_only = ch.telegram.mention_only;
        cfg.telegram = Some(ex);
    } else {
        cfg.telegram = None;
    }

    // -- Discord --
    if ch.discord.enabled {
        let mut ex = cfg.discord.take().unwrap_or_else(|| DiscordConfig {
            bot_token: String::new(),
            guild_id: None,
            allowed_users: Vec::new(),
            listen_to_bots: false,
            mention_only: false,
            group_reply: None,
        });
        if let Some(v) = unmask(&ch.discord.bot_token) {
            ex.bot_token = v;
        }
        ex.guild_id = if ch.discord.guild_id.is_empty() {
            None
        } else {
            Some(ch.discord.guild_id.clone())
        };
        ex.allowed_users = split_list(&ch.discord.allowed_users);
        ex.listen_to_bots = ch.discord.listen_to_bots;
        ex.mention_only = ch.discord.mention_only;
        cfg.discord = Some(ex);
    } else {
        cfg.discord = None;
    }

    // -- Slack --
    if ch.slack.enabled {
        let mut ex = cfg.slack.take().unwrap_or_else(|| SlackConfig {
            bot_token: String::new(),
            app_token: None,
            channel_id: None,
            channel_ids: Vec::new(),
            allowed_users: Vec::new(),
            group_reply: None,
        });
        if let Some(v) = unmask(&ch.slack.bot_token) {
            ex.bot_token = v;
        }
        if let Some(v) = unmask(&ch.slack.app_token) {
            ex.app_token = Some(v);
        }
        ex.channel_id = if ch.slack.channel_id.is_empty() {
            None
        } else {
            Some(ch.slack.channel_id.clone())
        };
        ex.allowed_users = split_list(&ch.slack.allowed_users);
        cfg.slack = Some(ex);
    } else {
        cfg.slack = None;
    }

    // -- Mattermost --
    if ch.mattermost.enabled {
        let mut ex = cfg.mattermost.take().unwrap_or_else(|| MattermostConfig {
            url: String::new(),
            bot_token: String::new(),
            channel_id: None,
            allowed_users: Vec::new(),
            thread_replies: None,
            mention_only: None,
            group_reply: None,
        });
        ex.url = ch.mattermost.url.clone();
        if let Some(v) = unmask(&ch.mattermost.bot_token) {
            ex.bot_token = v;
        }
        ex.channel_id = if ch.mattermost.channel_id.is_empty() {
            None
        } else {
            Some(ch.mattermost.channel_id.clone())
        };
        ex.allowed_users = split_list(&ch.mattermost.allowed_users);
        ex.thread_replies = Some(ch.mattermost.thread_replies);
        ex.mention_only = Some(ch.mattermost.mention_only);
        cfg.mattermost = Some(ex);
    } else {
        cfg.mattermost = None;
    }

    // -- Webhook --
    if ch.webhook.enabled {
        let mut ex = cfg.webhook.take().unwrap_or_else(|| WebhookConfig {
            port: 8080,
            secret: None,
        });
        ex.port = ch.webhook.port;
        if let Some(v) = unmask(&ch.webhook.secret) {
            ex.secret = Some(v);
        }
        cfg.webhook = Some(ex);
    } else {
        cfg.webhook = None;
    }

    // -- iMessage --
    if ch.imessage.enabled {
        cfg.imessage = Some(IMessageConfig {
            allowed_contacts: split_list(&ch.imessage.allowed_contacts),
        });
    } else {
        cfg.imessage = None;
    }

    // -- Matrix --
    if ch.matrix.enabled {
        let mut ex = cfg.matrix.take().unwrap_or_else(|| MatrixConfig {
            homeserver: String::new(),
            access_token: String::new(),
            user_id: None,
            device_id: None,
            room_id: String::new(),
            allowed_users: Vec::new(),
            mention_only: false,
        });
        ex.homeserver = ch.matrix.homeserver.clone();
        if let Some(v) = unmask(&ch.matrix.access_token) {
            ex.access_token = v;
        }
        ex.user_id = if ch.matrix.user_id.is_empty() {
            None
        } else {
            Some(ch.matrix.user_id.clone())
        };
        ex.device_id = if ch.matrix.device_id.is_empty() {
            None
        } else {
            Some(ch.matrix.device_id.clone())
        };
        ex.room_id = ch.matrix.room_id.clone();
        ex.allowed_users = split_list(&ch.matrix.allowed_users);
        ex.mention_only = ch.matrix.mention_only;
        cfg.matrix = Some(ex);
    } else {
        cfg.matrix = None;
    }

    // -- Signal --
    if ch.signal.enabled {
        let mut ex = cfg.signal.take().unwrap_or_else(|| SignalConfig {
            http_url: "http://127.0.0.1:8686".to_string(),
            account: String::new(),
            group_id: None,
            allowed_from: Vec::new(),
            ignore_attachments: false,
            ignore_stories: true,
        });
        ex.http_url = ch.signal.http_url.clone();
        ex.account = ch.signal.account.clone();
        ex.group_id = if ch.signal.group_id.is_empty() {
            None
        } else {
            Some(ch.signal.group_id.clone())
        };
        ex.allowed_from = split_list(&ch.signal.allowed_from);
        ex.ignore_attachments = ch.signal.ignore_attachments;
        ex.ignore_stories = ch.signal.ignore_stories;
        cfg.signal = Some(ex);
    } else {
        cfg.signal = None;
    }

    // -- WhatsApp --
    if ch.whatsapp.enabled {
        let mut ex = cfg.whatsapp.take().unwrap_or_else(|| WhatsAppConfig {
            access_token: None,
            phone_number_id: None,
            verify_token: None,
            app_secret: None,
            session_path: None,
            pair_phone: None,
            pair_code: None,
            allowed_numbers: Vec::new(),
        });
        if let Some(v) = unmask(&ch.whatsapp.access_token) {
            ex.access_token = Some(v);
        }
        ex.phone_number_id = if ch.whatsapp.phone_number_id.is_empty() {
            None
        } else {
            Some(ch.whatsapp.phone_number_id.clone())
        };
        if let Some(v) = unmask(&ch.whatsapp.verify_token) {
            ex.verify_token = Some(v);
        }
        if let Some(v) = unmask(&ch.whatsapp.app_secret) {
            ex.app_secret = Some(v);
        }
        ex.session_path = if ch.whatsapp.session_path.is_empty() {
            None
        } else {
            Some(ch.whatsapp.session_path.clone())
        };
        ex.pair_phone = if ch.whatsapp.pair_phone.is_empty() {
            None
        } else {
            Some(ch.whatsapp.pair_phone.clone())
        };
        if let Some(v) = unmask(&ch.whatsapp.pair_code) {
            ex.pair_code = Some(v);
        }
        ex.allowed_numbers = split_list(&ch.whatsapp.allowed_numbers);
        cfg.whatsapp = Some(ex);
    } else {
        cfg.whatsapp = None;
    }

    // -- Linq --
    if ch.linq.enabled {
        let mut ex = cfg.linq.take().unwrap_or_else(|| LinqConfig {
            api_token: String::new(),
            from_phone: String::new(),
            signing_secret: None,
            allowed_senders: Vec::new(),
        });
        if let Some(v) = unmask(&ch.linq.api_token) {
            ex.api_token = v;
        }
        ex.from_phone = ch.linq.from_phone.clone();
        if let Some(v) = unmask(&ch.linq.signing_secret) {
            ex.signing_secret = Some(v);
        }
        ex.allowed_senders = split_list(&ch.linq.allowed_senders);
        cfg.linq = Some(ex);
    } else {
        cfg.linq = None;
    }

    // -- WATI --
    if ch.wati.enabled {
        let mut ex = cfg.wati.take().unwrap_or_else(|| WatiConfig {
            api_token: String::new(),
            api_url: String::new(),
            tenant_id: None,
            allowed_numbers: Vec::new(),
        });
        if let Some(v) = unmask(&ch.wati.api_token) {
            ex.api_token = v;
        }
        ex.api_url = ch.wati.api_url.clone();
        ex.tenant_id = if ch.wati.tenant_id.is_empty() {
            None
        } else {
            Some(ch.wati.tenant_id.clone())
        };
        ex.allowed_numbers = split_list(&ch.wati.allowed_numbers);
        cfg.wati = Some(ex);
    } else {
        cfg.wati = None;
    }

    // -- Nextcloud Talk --
    if ch.nextcloud_talk.enabled {
        let mut ex = cfg
            .nextcloud_talk
            .take()
            .unwrap_or_else(|| NextcloudTalkConfig {
                base_url: String::new(),
                app_token: String::new(),
                webhook_secret: None,
                allowed_users: Vec::new(),
            });
        ex.base_url = ch.nextcloud_talk.base_url.clone();
        if let Some(v) = unmask(&ch.nextcloud_talk.app_token) {
            ex.app_token = v;
        }
        if let Some(v) = unmask(&ch.nextcloud_talk.webhook_secret) {
            ex.webhook_secret = Some(v);
        }
        ex.allowed_users = split_list(&ch.nextcloud_talk.allowed_users);
        cfg.nextcloud_talk = Some(ex);
    } else {
        cfg.nextcloud_talk = None;
    }

    // -- IRC --
    if ch.irc.enabled {
        let mut ex = cfg.irc.take().unwrap_or_else(|| IrcConfig {
            server: String::new(),
            port: 6697,
            nickname: String::new(),
            username: None,
            channels: Vec::new(),
            allowed_users: Vec::new(),
            server_password: None,
            nickserv_password: None,
            sasl_password: None,
            verify_tls: Some(true),
        });
        ex.server = ch.irc.server.clone();
        ex.port = ch.irc.port;
        ex.nickname = ch.irc.nickname.clone();
        ex.username = if ch.irc.username.is_empty() {
            None
        } else {
            Some(ch.irc.username.clone())
        };
        ex.channels = split_list(&ch.irc.channels);
        ex.allowed_users = split_list(&ch.irc.allowed_users);
        if let Some(v) = unmask(&ch.irc.server_password) {
            ex.server_password = Some(v);
        }
        if let Some(v) = unmask(&ch.irc.nickserv_password) {
            ex.nickserv_password = Some(v);
        }
        if let Some(v) = unmask(&ch.irc.sasl_password) {
            ex.sasl_password = Some(v);
        }
        ex.verify_tls = Some(ch.irc.verify_tls);
        cfg.irc = Some(ex);
    } else {
        cfg.irc = None;
    }

    // -- Lark --
    if ch.lark.enabled {
        let mut ex = cfg.lark.take().unwrap_or_else(|| LarkConfig {
            app_id: String::new(),
            app_secret: String::new(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: Vec::new(),
            mention_only: false,
            use_feishu: false,
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: 3000,
            group_reply: None,
            max_draft_edits: 20,
        });
        ex.app_id = ch.lark.app_id.clone();
        if let Some(v) = unmask(&ch.lark.app_secret) {
            ex.app_secret = v;
        }
        if let Some(v) = unmask(&ch.lark.encrypt_key) {
            ex.encrypt_key = Some(v);
        }
        if let Some(v) = unmask(&ch.lark.verification_token) {
            ex.verification_token = Some(v);
        }
        ex.allowed_users = split_list(&ch.lark.allowed_users);
        ex.mention_only = ch.lark.mention_only;
        ex.use_feishu = ch.lark.use_feishu;
        ex.receive_mode = match ch.lark.receive_mode.as_str() {
            "webhook" => LarkReceiveMode::Webhook,
            _ => LarkReceiveMode::Websocket,
        };
        ex.port = if ch.lark.port == 0 {
            None
        } else {
            Some(ch.lark.port)
        };
        cfg.lark = Some(ex);
    } else {
        cfg.lark = None;
    }

    // -- Feishu --
    if ch.feishu.enabled {
        let mut ex = cfg.feishu.take().unwrap_or_else(|| FeishuConfig {
            app_id: String::new(),
            app_secret: String::new(),
            encrypt_key: None,
            verification_token: None,
            allowed_users: Vec::new(),
            receive_mode: LarkReceiveMode::Websocket,
            port: None,
            draft_update_interval_ms: 3000,
            group_reply: None,
            max_draft_edits: 20,
        });
        ex.app_id = ch.feishu.app_id.clone();
        if let Some(v) = unmask(&ch.feishu.app_secret) {
            ex.app_secret = v;
        }
        if let Some(v) = unmask(&ch.feishu.encrypt_key) {
            ex.encrypt_key = Some(v);
        }
        if let Some(v) = unmask(&ch.feishu.verification_token) {
            ex.verification_token = Some(v);
        }
        ex.allowed_users = split_list(&ch.feishu.allowed_users);
        ex.receive_mode = match ch.feishu.receive_mode.as_str() {
            "webhook" => LarkReceiveMode::Webhook,
            _ => LarkReceiveMode::Websocket,
        };
        ex.port = if ch.feishu.port == 0 {
            None
        } else {
            Some(ch.feishu.port)
        };
        cfg.feishu = Some(ex);
    } else {
        cfg.feishu = None;
    }

    // -- DingTalk --
    if ch.dingtalk.enabled {
        let mut ex = cfg.dingtalk.take().unwrap_or_else(|| DingTalkConfig {
            client_id: String::new(),
            client_secret: String::new(),
            allowed_users: Vec::new(),
        });
        ex.client_id = ch.dingtalk.client_id.clone();
        if let Some(v) = unmask(&ch.dingtalk.client_secret) {
            ex.client_secret = v;
        }
        ex.allowed_users = split_list(&ch.dingtalk.allowed_users);
        cfg.dingtalk = Some(ex);
    } else {
        cfg.dingtalk = None;
    }

    // -- QQ --
    if ch.qq.enabled {
        let mut ex = cfg.qq.take().unwrap_or_else(|| QQConfig {
            app_id: String::new(),
            app_secret: String::new(),
            allowed_users: Vec::new(),
            receive_mode: QQReceiveMode::default(),
            environment: QQEnvironment::default(),
        });
        ex.app_id = ch.qq.app_id.clone();
        if let Some(v) = unmask(&ch.qq.app_secret) {
            ex.app_secret = v;
        }
        ex.allowed_users = split_list(&ch.qq.allowed_users);
        ex.receive_mode = match ch.qq.receive_mode.as_str() {
            "websocket" => QQReceiveMode::Websocket,
            _ => QQReceiveMode::Webhook,
        };
        cfg.qq = Some(ex);
    } else {
        cfg.qq = None;
    }

    // -- Nostr --
    if ch.nostr.enabled {
        let mut ex = cfg.nostr.take().unwrap_or_else(|| NostrConfig {
            private_key: String::new(),
            relays: Vec::new(),
            allowed_pubkeys: Vec::new(),
        });
        if let Some(v) = unmask(&ch.nostr.private_key) {
            ex.private_key = v;
        }
        ex.relays = split_list(&ch.nostr.relays);
        ex.allowed_pubkeys = split_list(&ch.nostr.allowed_pubkeys);
        cfg.nostr = Some(ex);
    } else {
        cfg.nostr = None;
    }

    // -- Email --
    if ch.email.enabled {
        let mut ex = cfg.email.take().unwrap_or_else(|| EmailConfig {
            imap_host: String::new(),
            imap_port: 993,
            imap_folder: "INBOX".to_string(),
            smtp_host: String::new(),
            smtp_port: 587,
            smtp_tls: true,
            username: String::new(),
            password: String::new(),
            from_address: String::new(),
            idle_timeout_secs: 300,
            allowed_senders: Vec::new(),
            imap_id: EmailImapIdConfig::default(),
        });
        ex.imap_host = ch.email.imap_host.clone();
        ex.imap_port = ch.email.imap_port;
        ex.imap_folder = ch.email.imap_folder.clone();
        ex.smtp_host = ch.email.smtp_host.clone();
        ex.smtp_port = ch.email.smtp_port;
        ex.smtp_tls = ch.email.smtp_tls;
        ex.username = ch.email.username.clone();
        if let Some(v) = unmask(&ch.email.password) {
            ex.password = v;
        }
        ex.from_address = ch.email.from_address.clone();
        ex.idle_timeout_secs = ch.email.idle_timeout_secs;
        ex.allowed_senders = split_list(&ch.email.allowed_senders);
        cfg.email = Some(ex);
    } else {
        cfg.email = None;
    }

    // -- ClawdTalk --
    if ch.clawdtalk.enabled {
        let mut ex = cfg.clawdtalk.take().unwrap_or_else(|| ClawdTalkConfig {
            api_key: String::new(),
            connection_id: String::new(),
            from_number: String::new(),
            allowed_destinations: Vec::new(),
            webhook_secret: None,
        });
        if let Some(v) = unmask(&ch.clawdtalk.api_key) {
            ex.api_key = v;
        }
        ex.connection_id = ch.clawdtalk.connection_id.clone();
        ex.from_number = ch.clawdtalk.from_number.clone();
        ex.allowed_destinations = split_list(&ch.clawdtalk.allowed_destinations);
        if let Some(v) = unmask(&ch.clawdtalk.webhook_secret) {
            ex.webhook_secret = Some(v);
        }
        cfg.clawdtalk = Some(ex);
    } else {
        cfg.clawdtalk = None;
    }
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

fn extract_bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|auth| auth.strip_prefix("Bearer "))
}

fn require_auth(
    state: &OnboardState,
    headers: &HeaderMap,
) -> Result<(), (StatusCode, Json<serde_json::Value>)> {
    if !state.pairing.require_pairing() {
        return Ok(());
    }
    let token = extract_bearer_token(headers).unwrap_or("");
    if state.pairing.is_authenticated(token) {
        Ok(())
    } else {
        Err((
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({
                "error": "Unauthorized — pair first via POST /pair"
            })),
        ))
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn handle_health(State(state): State<OnboardState>) -> impl IntoResponse {
    Json(serde_json::json!({
        "status": "ok",
        "mode": "onboard",
        "require_pairing": state.pairing.require_pairing(),
        "paired": state.pairing.is_paired(),
    }))
}

/// Catch-all for unregistered `/api/*` paths — returns JSON 404 instead of
/// falling through to the SPA fallback (which would return HTML).
async fn handle_api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "This API endpoint is not available on the onboard server."
        })),
    )
}

async fn handle_pair(State(state): State<OnboardState>, headers: HeaderMap) -> impl IntoResponse {
    let code = headers
        .get("X-Pairing-Code")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    // Use a simple client id since we don't have ConnectInfo here
    let client_id = "onboard-client";

    match state.pairing.try_pair(code, client_id).await {
        Ok(Some(token)) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "paired": true,
                "token": token,
            })),
        )
            .into_response(),
        Ok(None) => (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({ "error": "Invalid pairing code" })),
        )
            .into_response(),
        Err(lockout_secs) => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(serde_json::json!({
                "error": format!("Too many failed attempts. Try again in {lockout_secs}s."),
                "retry_after": lockout_secs,
            })),
        )
            .into_response(),
    }
}

async fn handle_config_get(
    State(state): State<OnboardState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let config = state.config.lock();
    let mut setup = config_to_setup(&config);
    // Overlay session-held project context (not stored in config)
    setup.project_context = state.project_context.lock().clone();
    Json(setup).into_response()
}

async fn handle_config_put(
    State(state): State<OnboardState>,
    headers: HeaderMap,
    Json(setup): Json<SetupConfig>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    // Persist project context in session state
    *state.project_context.lock() = setup.project_context.clone();

    let save_result = {
        let mut config = state.config.lock();
        apply_setup_to_config(&setup, &mut config);

        if let Err(e) = config.validate() {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({ "error": format!("{e:#}") })),
            )
                .into_response();
        }

        // Clone for async save (release lock before awaiting)
        config.clone()
    };

    match save_result.save().await {
        Ok(()) => Json(serde_json::json!({ "ok": true })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to save: {e:#}") })),
        )
            .into_response(),
    }
}

async fn handle_providers(
    State(state): State<OnboardState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    Json(
        list_providers()
            .into_iter()
            .map(|p| {
                serde_json::json!({
                    "name": p.name,
                    "display_name": p.display_name,
                    "local": p.local,
                })
            })
            .collect::<Vec<_>>(),
    )
    .into_response()
}

async fn handle_models(
    State(state): State<OnboardState>,
    headers: HeaderMap,
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }
    let provider = params
        .get("provider")
        .map(|s| s.as_str())
        .unwrap_or("openrouter");

    let curated = crate::onboard::wizard::curated_models_for_provider(provider);
    let default_model = crate::onboard::wizard::default_model_for_provider(provider);

    Json(serde_json::json!({
        "default": default_model,
        "models": curated.into_iter().map(|(id, label)| {
            serde_json::json!({ "id": id, "label": label })
        }).collect::<Vec<_>>(),
    }))
    .into_response()
}

async fn handle_scaffold(
    State(state): State<OnboardState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Err(e) = require_auth(&state, &headers) {
        return e.into_response();
    }

    let (workspace_dir, memory_backend, identity_config) = {
        let config = state.config.lock();
        (
            config.workspace_dir.clone(),
            config.memory.backend.clone(),
            config.identity.clone(),
        )
    };
    let ctx_setup = state.project_context.lock().clone();

    let ctx = crate::onboard::wizard::ProjectContext {
        user_name: ctx_setup.user_name,
        timezone: ctx_setup.timezone,
        agent_name: ctx_setup.agent_name,
        communication_style: ctx_setup.communication_style,
    };

    match crate::onboard::wizard::scaffold_workspace(&workspace_dir, &ctx, &memory_backend, &identity_config).await {
        Ok(()) => Json(serde_json::json!({
            "ok": true,
            "message": format!("Workspace files created in {}", workspace_dir.display()),
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Scaffold failed: {e:#}") })),
        )
            .into_response(),
    }
}

// ---------------------------------------------------------------------------
// Server entry point
// ---------------------------------------------------------------------------

/// Returns `true` if `host` is a loopback address.
fn is_localhost(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "::1" | "localhost")
}

/// Launch the standalone onboard web server.
///
/// Binds to `<host>:<port>`, serves the embedded SPA, and exposes
/// JSON APIs for the Setup page. Ctrl+C stops the server.
///
/// When `host` is non-localhost, a one-time pairing code is required
/// (same mechanism as the gateway) to protect against unauthorized access.
pub async fn run_onboard_web(config: Config, host: &str, port: u16) -> Result<()> {
    let require_pairing = !is_localhost(host);
    let pairing = Arc::new(PairingGuard::new(require_pairing, &[]));

    let state = OnboardState {
        config: Arc::new(Mutex::new(config)),
        pairing: pairing.clone(),
        project_context: Arc::new(Mutex::new(ProjectContextSetup::default())),
    };

    // API routes in a nested router so that unregistered /api/* paths get
    // a JSON 404 instead of falling through to the SPA HTML fallback.
    let api_routes = Router::new()
        .route(
            "/onboard/config",
            get(handle_config_get).put(handle_config_put),
        )
        .route("/onboard/providers", get(handle_providers))
        .route("/onboard/models", get(handle_models))
        .route("/onboard/scaffold", post(handle_scaffold))
        .fallback(handle_api_not_found)
        .with_state(state.clone());

    let app = Router::new()
        .route("/health", get(handle_health))
        .route("/pair", post(handle_pair))
        .nest("/api", api_routes)
        .route(
            "/_app/{*path}",
            get(crate::gateway::static_files::handle_static),
        )
        .with_state(state)
        .fallback(get(crate::gateway::static_files::handle_spa_fallback));

    let addr = format!("{host}:{port}");
    let listener = TcpListener::bind(&addr)
        .await
        .with_context(|| format!("Failed to bind onboard server to {addr}"))?;

    let url = format!("http://{addr}/setup");
    info!("Onboard web wizard running at {url}");

    // Print pairing code when binding to non-localhost
    if let Some(code) = pairing.pairing_code() {
        println!();
        println!("  🔐 PAIRING REQUIRED — use this one-time code:");
        println!("     ┌──────────────┐");
        println!("     │  {code}  │");
        println!("     └──────────────┘");
        println!();
    }

    println!("Press Ctrl+C to finish and save configuration.\n");

    // Best-effort browser open (skip on headless)
    if open_browser(&url) {
        println!("Opened {url} in your browser.");
    } else {
        println!("Open {url} in your browser to configure ZeroClaw.");
    }

    // Graceful shutdown on Ctrl+C
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("Onboard web server error")?;

    println!("\nOnboard server stopped.");
    Ok(())
}

async fn shutdown_signal() {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to install Ctrl+C handler");
}

/// Try to open `url` in the default browser. Returns `true` on success.
///
/// On headless Linux (no `$DISPLAY` / `$WAYLAND_DISPLAY`) we skip the attempt
/// entirely to avoid noisy "no DISPLAY" errors from `xdg-open`.
fn open_browser(url: &str) -> bool {
    use std::process::{Command, Stdio};

    #[cfg(target_os = "linux")]
    {
        let has_display =
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        if !has_display {
            return false;
        }
        Command::new("xdg-open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(url)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", url])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        false
    }
}
