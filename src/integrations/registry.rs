use super::{IntegrationCategory, IntegrationEntry, IntegrationStatus};
use crate::providers::{
    is_doubao_alias, is_glm_alias, is_minimax_alias, is_moonshot_alias, is_qianfan_alias,
    is_qwen_alias, is_siliconflow_alias, is_zai_alias,
};

/// Returns the full catalog of integrations
#[allow(clippy::too_many_lines)]
pub fn all_integrations() -> Vec<IntegrationEntry> {
    vec![
        // ── Chat Providers ──────────────────────────────────────
        IntegrationEntry {
            name: "Telegram",
            description: "Bot API — long-polling",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.telegram.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Discord",
            description: "Servers, channels & DMs",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.discord.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Slack",
            description: "Workspace apps via Web API",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.slack.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Webhooks",
            description: "HTTP endpoint for triggers",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.webhook.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "WhatsApp",
            description: "Meta Cloud API via webhook",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.whatsapp.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Signal",
            description: "Privacy-focused via signal-cli",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.signal.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "iMessage",
            description: "macOS AppleScript bridge",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.imessage.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Microsoft Teams",
            description: "Enterprise chat support",
            category: IntegrationCategory::Chat,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Matrix",
            description: "Matrix protocol (Element)",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.matrix.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Nostr",
            description: "Decentralized DMs (NIP-04)",
            category: IntegrationCategory::Chat,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "WebChat",
            description: "Browser-based chat UI",
            category: IntegrationCategory::Chat,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Nextcloud Talk",
            description: "Self-hosted Nextcloud chat",
            category: IntegrationCategory::Chat,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Zalo",
            description: "Zalo Bot API",
            category: IntegrationCategory::Chat,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "DingTalk",
            description: "DingTalk Stream Mode",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.dingtalk.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "QQ Official",
            description: "Tencent QQ Bot SDK",
            category: IntegrationCategory::Chat,
            status_fn: |c| {
                if c.channels_config.qq.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        // ── AI Models ───────────────────────────────────────────
        IntegrationEntry {
            name: "OpenRouter",
            description: "Claude Sonnet 4.6, GPT-5.2, Gemini 3.1 Pro",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("openrouter") && c.api_key.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Anthropic",
            description: "Claude Sonnet 4.6, Claude Opus 4.6",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("anthropic") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "OpenAI",
            description: "GPT-5.2, GPT-5.2-Codex",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("openai") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Google",
            description: "Gemini 3.1 Pro, Gemini 3 Flash",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_model
                    .as_deref()
                    .is_some_and(|m| m.starts_with("google/"))
                {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "DeepSeek",
            description: "DeepSeek-Reasoner, DeepSeek-Chat",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_model
                    .as_deref()
                    .is_some_and(|m| m.starts_with("deepseek/"))
                {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "xAI",
            description: "Grok 4, Grok 3",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_model
                    .as_deref()
                    .is_some_and(|m| m.starts_with("x-ai/"))
                {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Mistral",
            description: "Mistral Large Latest, Codestral",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_model
                    .as_deref()
                    .is_some_and(|m| m.starts_with("mistral"))
                {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Ollama",
            description: "Local models (Llama, etc.)",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("ollama") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Perplexity",
            description: "Sonar Pro, Sonar Reasoning Pro",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("perplexity") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Hugging Face",
            description: "Open-source models",
            category: IntegrationCategory::AiModel,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "LM Studio",
            description: "Local model server",
            category: IntegrationCategory::AiModel,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Venice",
            description: "Venice Llama 3.3 70B and frontier blends",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("venice") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Vercel AI",
            description: "Gateway for GPT-5.2 and multi-provider routing",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("vercel") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Cloudflare AI",
            description: "Workers AI + Llama 3.3 / gateway routing",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("cloudflare") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Moonshot",
            description: "Kimi 2.5 and Kimi Coding",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_moonshot_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Synthetic",
            description: "Synthetic-1 and synthetic family models",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("synthetic") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "OpenCode Zen",
            description: "OpenCode Zen and coding-specialized models",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("opencode") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Z.AI",
            description: "GLM 4.7 and Z.AI hosted variants",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_zai_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "GLM",
            description: "GLM 4.7 and GLM 4.5 family",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_glm_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "MiniMax",
            description: "MiniMax M1 and latest multimodal variants",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_minimax_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Qwen",
            description: "Qwen Max and Qwen reasoning families",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_qwen_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Amazon Bedrock",
            description: "Claude Sonnet 4.5 and Bedrock model catalog",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("bedrock") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Qianfan",
            description: "ERNIE 4.x and Qianfan model catalog",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_qianfan_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Volcengine ARK",
            description: "Doubao and ARK model catalog",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref().is_some_and(is_doubao_alias) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "SiliconFlow",
            description: "OpenAI-compatible hosted models and reasoning",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider
                    .as_deref()
                    .is_some_and(is_siliconflow_alias)
                {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Groq",
            description: "Llama 3.3 70B Versatile and low-latency models",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("groq") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Together AI",
            description: "Llama 3.3 70B Turbo and open model hosting",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("together") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Fireworks AI",
            description: "DeepSeek / Llama high-throughput inference",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("fireworks") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Cohere",
            description: "Command R+ (08-2024) and embedding models",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if c.default_provider.as_deref() == Some("cohere") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        // ── Productivity ────────────────────────────────────────
        IntegrationEntry {
            name: "GitHub",
            description: "Code, issues, PRs",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Notion",
            description: "Workspace & databases",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Apple Notes",
            description: "Native macOS/iOS notes",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Apple Reminders",
            description: "Task management",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Obsidian",
            description: "Knowledge graph notes",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Things 3",
            description: "GTD task manager",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Bear Notes",
            description: "Markdown notes",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Trello",
            description: "Kanban boards",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Linear",
            description: "Issue tracking",
            category: IntegrationCategory::Productivity,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        // ── Music & Audio ───────────────────────────────────────
        IntegrationEntry {
            name: "Spotify",
            description: "Music playback control",
            category: IntegrationCategory::MusicAudio,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Sonos",
            description: "Multi-room audio",
            category: IntegrationCategory::MusicAudio,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Shazam",
            description: "Song recognition",
            category: IntegrationCategory::MusicAudio,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        // ── Smart Home ──────────────────────────────────────────
        IntegrationEntry {
            name: "Home Assistant",
            description: "Home automation hub",
            category: IntegrationCategory::SmartHome,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Philips Hue",
            description: "Smart lighting",
            category: IntegrationCategory::SmartHome,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "8Sleep",
            description: "Smart mattress",
            category: IntegrationCategory::SmartHome,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        // ── Tools & Automation ──────────────────────────────────
        IntegrationEntry {
            name: "Browser",
            description: "Chrome/Chromium control",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::Available,
        },
        IntegrationEntry {
            name: "Shell",
            description: "Terminal command execution",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::Active,
        },
        IntegrationEntry {
            name: "File System",
            description: "Read/write files",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::Active,
        },
        IntegrationEntry {
            name: "Cron",
            description: "Scheduled tasks",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::Available,
        },
        IntegrationEntry {
            name: "Voice",
            description: "Voice wake + talk mode",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Gmail",
            description: "Email triggers & send",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "1Password",
            description: "Secure credentials",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Weather",
            description: "Forecasts & conditions",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Canvas",
            description: "Visual workspace + A2UI",
            category: IntegrationCategory::ToolsAutomation,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        // ── Media & Creative ────────────────────────────────────
        IntegrationEntry {
            name: "Image Gen",
            description: "AI image generation",
            category: IntegrationCategory::MediaCreative,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "GIF Search",
            description: "Find the perfect GIF",
            category: IntegrationCategory::MediaCreative,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Screen Capture",
            description: "Screenshot & screen control",
            category: IntegrationCategory::MediaCreative,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Camera",
            description: "Photo/video capture",
            category: IntegrationCategory::MediaCreative,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        // ── Social ──────────────────────────────────────────────
        IntegrationEntry {
            name: "Twitter/X",
            description: "Tweet, reply, search",
            category: IntegrationCategory::Social,
            status_fn: |_| IntegrationStatus::ComingSoon,
        },
        IntegrationEntry {
            name: "Email",
            description: "IMAP/SMTP email channel",
            category: IntegrationCategory::Social,
            status_fn: |c| {
                if c.channels_config.email.is_some() {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        // ── Platforms ───────────────────────────────────────────
        IntegrationEntry {
            name: "macOS",
            description: "Native support + AppleScript",
            category: IntegrationCategory::Platform,
            status_fn: |_| {
                if cfg!(target_os = "macos") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Linux",
            description: "Native support",
            category: IntegrationCategory::Platform,
            status_fn: |_| {
                if cfg!(target_os = "linux") {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        IntegrationEntry {
            name: "Windows",
            description: "WSL2 recommended",
            category: IntegrationCategory::Platform,
            status_fn: |_| IntegrationStatus::Available,
        },
        IntegrationEntry {
            name: "iOS",
            description: "Chat via Telegram/Discord",
            category: IntegrationCategory::Platform,
            status_fn: |_| IntegrationStatus::Available,
        },
        IntegrationEntry {
            name: "Android",
            description: "Chat via Telegram/Discord",
            category: IntegrationCategory::Platform,
            status_fn: |_| IntegrationStatus::Available,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{IMessageConfig, MatrixConfig, StreamMode, TelegramConfig};
    use crate::config::Config;

    #[test]
    fn registry_has_entries() {
        let entries = all_integrations();
        assert!(
            entries.len() >= 50,
            "Expected 50+ integrations, got {}",
            entries.len()
        );
    }

    #[test]
    fn all_categories_represented() {
        let entries = all_integrations();
        for cat in IntegrationCategory::all() {
            let count = entries.iter().filter(|e| e.category == *cat).count();
            assert!(count > 0, "Category {cat:?} has no entries");
        }
    }

    #[test]
    fn status_functions_dont_panic() {
        let config = Config::default();
        let entries = all_integrations();
        for entry in &entries {
            let _ = (entry.status_fn)(&config);
        }
    }

    #[test]
    fn no_duplicate_names() {
        let entries = all_integrations();
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                seen.insert(entry.name),
                "Duplicate integration name: {}",
                entry.name
            );
        }
    }

    #[test]
    fn no_empty_names_or_descriptions() {
        let entries = all_integrations();
        for entry in &entries {
            assert!(!entry.name.is_empty(), "Found integration with empty name");
            assert!(
                !entry.description.is_empty(),
                "Integration '{}' has empty description",
                entry.name
            );
        }
    }

    #[test]
    fn telegram_active_when_configured() {
        let mut config = Config::default();
        config.channels_config.telegram = Some(TelegramConfig {
            bot_token: "123:ABC".into(),
            allowed_users: vec!["user".into()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            group_reply: None,
            base_url: None,
        });
        let entries = all_integrations();
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!((tg.status_fn)(&config), IntegrationStatus::Active));
    }

    #[test]
    fn telegram_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations();
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!(
            (tg.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn imessage_active_when_configured() {
        let mut config = Config::default();
        config.channels_config.imessage = Some(IMessageConfig {
            allowed_contacts: vec!["*".into()],
        });
        let entries = all_integrations();
        let im = entries.iter().find(|e| e.name == "iMessage").unwrap();
        assert!(matches!((im.status_fn)(&config), IntegrationStatus::Active));
    }

    #[test]
    fn imessage_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations();
        let im = entries.iter().find(|e| e.name == "iMessage").unwrap();
        assert!(matches!(
            (im.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn matrix_active_when_configured() {
        let mut config = Config::default();
        config.channels_config.matrix = Some(MatrixConfig {
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            room_id: "!r:m".into(),
            allowed_users: vec![],
            mention_only: false,
        });
        let entries = all_integrations();
        let mx = entries.iter().find(|e| e.name == "Matrix").unwrap();
        assert!(matches!((mx.status_fn)(&config), IntegrationStatus::Active));
    }

    #[test]
    fn matrix_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations();
        let mx = entries.iter().find(|e| e.name == "Matrix").unwrap();
        assert!(matches!(
            (mx.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn coming_soon_integrations_stay_coming_soon() {
        let config = Config::default();
        let entries = all_integrations();
        for name in ["Nostr", "Spotify", "Home Assistant"] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert!(
                matches!((entry.status_fn)(&config), IntegrationStatus::ComingSoon),
                "{name} should be ComingSoon"
            );
        }
    }

    #[test]
    fn whatsapp_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations();
        let wa = entries.iter().find(|e| e.name == "WhatsApp").unwrap();
        assert!(matches!(
            (wa.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn email_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations();
        let email = entries.iter().find(|e| e.name == "Email").unwrap();
        assert!(matches!(
            (email.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn shell_and_filesystem_always_active() {
        let config = Config::default();
        let entries = all_integrations();
        for name in ["Shell", "File System"] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert!(
                matches!((entry.status_fn)(&config), IntegrationStatus::Active),
                "{name} should always be Active"
            );
        }
    }

    #[test]
    fn macos_active_on_macos() {
        let config = Config::default();
        let entries = all_integrations();
        let macos = entries.iter().find(|e| e.name == "macOS").unwrap();
        let status = (macos.status_fn)(&config);
        if cfg!(target_os = "macos") {
            assert!(matches!(status, IntegrationStatus::Active));
        } else {
            assert!(matches!(status, IntegrationStatus::Available));
        }
    }

    #[test]
    fn category_counts_reasonable() {
        let entries = all_integrations();
        let chat_count = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::Chat)
            .count();
        let ai_count = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::AiModel)
            .count();
        assert!(
            chat_count >= 5,
            "Expected 5+ chat integrations, got {chat_count}"
        );
        assert!(
            ai_count >= 5,
            "Expected 5+ AI model integrations, got {ai_count}"
        );
    }

    #[test]
    fn regional_provider_aliases_activate_expected_ai_integrations() {
        let entries = all_integrations();
        let mut config = Config {
            default_provider: Some("minimax-cn".to_string()),
            ..Config::default()
        };

        let minimax = entries.iter().find(|e| e.name == "MiniMax").unwrap();
        assert!(matches!(
            (minimax.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("glm-cn".to_string());
        let glm = entries.iter().find(|e| e.name == "GLM").unwrap();
        assert!(matches!(
            (glm.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("moonshot-intl".to_string());
        let moonshot = entries.iter().find(|e| e.name == "Moonshot").unwrap();
        assert!(matches!(
            (moonshot.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("qwen-intl".to_string());
        let qwen = entries.iter().find(|e| e.name == "Qwen").unwrap();
        assert!(matches!(
            (qwen.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("zai-cn".to_string());
        let zai = entries.iter().find(|e| e.name == "Z.AI").unwrap();
        assert!(matches!(
            (zai.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("baidu".to_string());
        let qianfan = entries.iter().find(|e| e.name == "Qianfan").unwrap();
        assert!(matches!(
            (qianfan.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("ark".to_string());
        let volcengine = entries.iter().find(|e| e.name == "Volcengine ARK").unwrap();
        assert!(matches!(
            (volcengine.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("volcengine".to_string());
        assert!(matches!(
            (volcengine.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("siliconflow".to_string());
        let siliconflow = entries.iter().find(|e| e.name == "SiliconFlow").unwrap();
        assert!(matches!(
            (siliconflow.status_fn)(&config),
            IntegrationStatus::Active
        ));

        config.default_provider = Some("silicon-cloud".to_string());
        assert!(matches!(
            (siliconflow.status_fn)(&config),
            IntegrationStatus::Active
        ));
    }
}
