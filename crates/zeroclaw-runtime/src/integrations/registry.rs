//! Integration catalog.
//!
//! Every entry corresponds to either:
//! - a schema field (chat channels via `ChannelsConfig::nested_option_entries`,
//!   AI providers via `providers.fallback`), or
//! - a runtime built-in (`Shell`, `File System`, `Weather`, `Browser`, `Cron`,
//!   `Google Workspace`), or
//! - a compile-time platform fact (`cfg!(target_os = ...)`).
//!
//! There is no hand-maintained channel list. Adding a
//! `pub foo: Option<FooConfig>` field with `#[nested]` to `ChannelsConfig`
//! surfaces a `Foo` integration entry automatically; no edit here required.
//! There is also no "coming soon" status — if it is not in the schema or
//! a real built-in, it does not get listed.

use super::{IntegrationCategory, IntegrationEntry, IntegrationStatus};
use zeroclaw_config::schema::Config;
use zeroclaw_providers::{
    is_glm_alias, is_minimax_alias, is_moonshot_alias, is_qianfan_alias, is_qwen_alias,
    is_zai_alias,
};

/// Map snake_case schema field names to display names. Anything not in
/// this table title-cases the snake_case name (e.g. `discord_history`
/// becomes "Discord History"). Override here when title-case looks
/// wrong (acronyms, brand casing).
fn channel_display_name(field: &str) -> String {
    match field {
        "imessage" => "iMessage".into(),
        "qq" => "QQ Official".into(),
        "irc" => "IRC".into(),
        "mqtt" => "MQTT".into(),
        "wati" => "WATI".into(),
        "wecom" => "WeCom".into(),
        "wechat" => "WeChat".into(),
        "dingtalk" => "DingTalk".into(),
        "whatsapp" => "WhatsApp".into(),
        "twitter" => "X / Twitter".into(),
        "bluesky" => "Bluesky".into(),
        "clawdtalk" => "ClawdTalk".into(),
        "voice_call" => "Voice Call".into(),
        "voice_wake" => "Voice Wake".into(),
        "voice_duplex" => "Voice Duplex".into(),
        "gmail_push" => "Gmail Push".into(),
        "nextcloud_talk" => "Nextcloud Talk".into(),
        "discord_history" => "Discord History".into(),
        other => snake_to_title(other),
    }
}

fn snake_to_title(s: &str) -> String {
    s.split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `status_fn` for an AI-model integration keyed off
/// `providers.fallback`. Accepts one or more literal provider keys
/// (e.g. `provider_active!("huggingface", "hf")`).
macro_rules! provider_active {
    ($c:expr, $($name:expr),+ $(,)?) => {
        match $c.providers.fallback.as_deref() {
            $(Some($name))|+ => IntegrationStatus::Active,
            _ => IntegrationStatus::Available,
        }
    };
}

/// `status_fn` for an AI-model integration whose provider key is
/// recognised by an alias predicate (e.g. `is_moonshot_alias`).
macro_rules! provider_alias_active {
    ($c:expr, $alias_fn:path) => {
        if $c.providers.fallback.as_deref().is_some_and($alias_fn) {
            IntegrationStatus::Active
        } else {
            IntegrationStatus::Available
        }
    };
}

/// `status_fn` for an AI-model integration recognised by a model-id
/// prefix on the resolved fallback provider (e.g. `"google/"`).
macro_rules! provider_model_prefix_active {
    ($c:expr, $prefix:expr) => {
        if $c
            .providers
            .fallback_provider()
            .and_then(|e| e.model.as_deref())
            .is_some_and(|m| m.starts_with($prefix))
        {
            IntegrationStatus::Active
        } else {
            IntegrationStatus::Available
        }
    };
}

fn bool_to_status(active: bool) -> IntegrationStatus {
    if active {
        IntegrationStatus::Active
    } else {
        IntegrationStatus::Available
    }
}

fn entry(
    name: impl Into<String>,
    description: impl Into<String>,
    category: IntegrationCategory,
    status: IntegrationStatus,
) -> IntegrationEntry {
    IntegrationEntry {
        name: name.into(),
        description: description.into(),
        category,
        status,
    }
}

/// Returns the integration catalog computed against `config`.
pub fn all_integrations(config: &Config) -> Vec<IntegrationEntry> {
    let mut out: Vec<IntegrationEntry> = Vec::new();

    // ── Chat channels (schema-derived) ──
    // Every `#[nested] Option<XConfig>` field on `ChannelsConfig`
    // surfaces here. No hand-list, no ComingSoon stand-ins.
    out.push(entry(
        "CLI",
        "In-process interactive shell",
        IntegrationCategory::Chat,
        bool_to_status(config.channels.cli),
    ));
    for (field, is_set) in config.channels.nested_option_entries() {
        out.push(entry(
            channel_display_name(field),
            "Chat channel",
            IntegrationCategory::Chat,
            bool_to_status(is_set),
        ));
    }

    // ── AI Models ──
    // Provider keys live in the schema (`providers.fallback`); the lookup
    // macros derive status from there.
    let openrouter_active = config.providers.fallback.as_deref() == Some("openrouter")
        && config
            .providers
            .fallback_provider()
            .and_then(|e| e.api_key.as_ref())
            .is_some();
    out.push(entry(
        "OpenRouter",
        "200+ models, 1 API key",
        IntegrationCategory::AiModel,
        bool_to_status(openrouter_active),
    ));
    out.push(entry(
        "Anthropic",
        "Claude 3.5/4 Sonnet & Opus",
        IntegrationCategory::AiModel,
        provider_active!(config, "anthropic"),
    ));
    out.push(entry(
        "OpenAI",
        "GPT-4o, GPT-5, o1",
        IntegrationCategory::AiModel,
        provider_active!(config, "openai"),
    ));
    out.push(entry(
        "Google",
        "Gemini 2.5 Pro/Flash",
        IntegrationCategory::AiModel,
        provider_model_prefix_active!(config, "google/"),
    ));
    out.push(entry(
        "DeepSeek",
        "DeepSeek V3 & R1",
        IntegrationCategory::AiModel,
        provider_model_prefix_active!(config, "deepseek/"),
    ));
    out.push(entry(
        "xAI",
        "Grok 3 & 4",
        IntegrationCategory::AiModel,
        provider_model_prefix_active!(config, "x-ai/"),
    ));
    out.push(entry(
        "Mistral",
        "Mistral Large & Codestral",
        IntegrationCategory::AiModel,
        provider_model_prefix_active!(config, "mistral"),
    ));
    out.push(entry(
        "Ollama",
        "Local models (Llama, etc.)",
        IntegrationCategory::AiModel,
        provider_active!(config, "ollama"),
    ));
    out.push(entry(
        "Perplexity",
        "Search-augmented AI",
        IntegrationCategory::AiModel,
        provider_active!(config, "perplexity"),
    ));
    out.push(entry(
        "Hugging Face",
        "Open-source models",
        IntegrationCategory::AiModel,
        provider_active!(config, "huggingface", "hf"),
    ));
    out.push(entry(
        "LM Studio",
        "Local model server",
        IntegrationCategory::AiModel,
        provider_active!(config, "lmstudio", "lm-studio"),
    ));
    out.push(entry(
        "Venice",
        "Privacy-first inference (Llama, Opus)",
        IntegrationCategory::AiModel,
        provider_active!(config, "venice"),
    ));
    out.push(entry(
        "Vercel AI",
        "Vercel AI Gateway",
        IntegrationCategory::AiModel,
        provider_active!(config, "vercel"),
    ));
    out.push(entry(
        "Cloudflare AI",
        "Cloudflare AI Gateway",
        IntegrationCategory::AiModel,
        provider_active!(config, "cloudflare"),
    ));
    out.push(entry(
        "Moonshot",
        "Kimi & Kimi Coding",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_moonshot_alias),
    ));
    out.push(entry(
        "Synthetic",
        "Synthetic AI models",
        IntegrationCategory::AiModel,
        provider_active!(config, "synthetic"),
    ));
    out.push(entry(
        "OpenCode Zen",
        "Code-focused AI models",
        IntegrationCategory::AiModel,
        provider_active!(config, "opencode"),
    ));
    out.push(entry(
        "OpenCode Go",
        "Subsidized code-focused AI models",
        IntegrationCategory::AiModel,
        provider_active!(config, "opencode-go"),
    ));
    out.push(entry(
        "Z.AI",
        "Z.AI inference",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_zai_alias),
    ));
    out.push(entry(
        "GLM",
        "ChatGLM / Zhipu models",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_glm_alias),
    ));
    out.push(entry(
        "MiniMax",
        "MiniMax AI models",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_minimax_alias),
    ));
    out.push(entry(
        "Qwen",
        "Alibaba DashScope Qwen models",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_qwen_alias),
    ));
    out.push(entry(
        "Amazon Bedrock",
        "AWS managed model access",
        IntegrationCategory::AiModel,
        provider_active!(config, "bedrock"),
    ));
    out.push(entry(
        "Qianfan",
        "Baidu AI models",
        IntegrationCategory::AiModel,
        provider_alias_active!(config, is_qianfan_alias),
    ));
    out.push(entry(
        "Groq",
        "Ultra-fast LPU inference",
        IntegrationCategory::AiModel,
        provider_active!(config, "groq"),
    ));
    out.push(entry(
        "Together AI",
        "Open-source model hosting",
        IntegrationCategory::AiModel,
        provider_active!(config, "together"),
    ));
    out.push(entry(
        "Fireworks AI",
        "Fast open-source inference",
        IntegrationCategory::AiModel,
        provider_active!(config, "fireworks"),
    ));
    out.push(entry(
        "Novita AI",
        "Affordable open-source inference",
        IntegrationCategory::AiModel,
        provider_active!(config, "novita"),
    ));
    out.push(entry(
        "Cohere",
        "Command R+ & embeddings",
        IntegrationCategory::AiModel,
        provider_active!(config, "cohere"),
    ));

    // ── Tools & Automation (runtime built-ins + a few schema bools) ──
    out.push(entry(
        "Browser",
        "Chrome/Chromium control",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.browser.enabled),
    ));
    out.push(entry(
        "Cron",
        "Scheduled tasks",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.cron.enabled),
    ));
    out.push(entry(
        "Google Workspace",
        "Drive, Gmail, Calendar, Sheets, Docs via gws CLI",
        IntegrationCategory::ToolsAutomation,
        bool_to_status(config.google_workspace.enabled),
    ));
    out.push(entry(
        "Shell",
        "Terminal command execution",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));
    out.push(entry(
        "File System",
        "Read/write files",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));
    out.push(entry(
        "Weather",
        "Forecasts & conditions (wttr.in)",
        IntegrationCategory::ToolsAutomation,
        IntegrationStatus::Active,
    ));

    // ── Platforms (compile-time facts) ──
    out.push(entry(
        "macOS",
        "Native support + AppleScript",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "macos")),
    ));
    out.push(entry(
        "Linux",
        "Native support",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "linux")),
    ));
    out.push(entry(
        "Windows",
        "Native support (WSL2 recommended)",
        IntegrationCategory::Platform,
        bool_to_status(cfg!(target_os = "windows")),
    ));

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::Config;
    use zeroclaw_config::schema::{IMessageConfig, MatrixConfig, StreamMode, TelegramConfig};

    #[test]
    fn registry_has_entries() {
        let config = Config::default();
        let entries = all_integrations(&config);
        assert!(
            entries.len() >= 30,
            "Expected 30+ integrations, got {}",
            entries.len()
        );
    }

    #[test]
    fn all_categories_represented() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for cat in IntegrationCategory::all() {
            let count = entries.iter().filter(|e| e.category == *cat).count();
            assert!(count > 0, "Category {cat:?} has no entries");
        }
    }

    #[test]
    fn no_duplicate_names() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let mut seen = std::collections::HashSet::new();
        for entry in &entries {
            assert!(
                seen.insert(entry.name.clone()),
                "Duplicate integration name: {}",
                entry.name
            );
        }
    }

    #[test]
    fn no_empty_names_or_descriptions() {
        let config = Config::default();
        let entries = all_integrations(&config);
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
        config.channels.telegram = Some(TelegramConfig {
            enabled: true,
            bot_token: "123:ABC".into(),
            allowed_users: vec!["user".into()],
            stream_mode: StreamMode::default(),
            draft_update_interval_ms: 1000,
            interrupt_on_new_message: false,
            mention_only: false,
            ack_reactions: None,
            proxy_url: None,
            approval_timeout_secs: 120,
        });
        let entries = all_integrations(&config);
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Active));
    }

    #[test]
    fn telegram_available_when_not_configured() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let tg = entries.iter().find(|e| e.name == "Telegram").unwrap();
        assert!(matches!(tg.status, IntegrationStatus::Available));
    }

    #[test]
    fn imessage_active_when_configured_and_displays_brand_casing() {
        let mut config = Config::default();
        config.channels.imessage = Some(IMessageConfig {
            enabled: true,
            allowed_contacts: vec!["*".into()],
        });
        let entries = all_integrations(&config);
        let im = entries.iter().find(|e| e.name == "iMessage").unwrap();
        assert!(matches!(im.status, IntegrationStatus::Active));
    }

    #[test]
    fn matrix_active_when_configured() {
        let mut config = Config::default();
        config.channels.matrix = Some(MatrixConfig {
            enabled: true,
            homeserver: "https://m.org".into(),
            access_token: "tok".into(),
            user_id: None,
            device_id: None,
            allowed_users: vec![],
            allowed_rooms: vec!["!r:m".into()],
            interrupt_on_new_message: false,
            stream_mode: zeroclaw_config::schema::StreamMode::default(),
            draft_update_interval_ms: 1500,
            multi_message_delay_ms: 800,
            recovery_key: None,
            password: None,
            mention_only: false,
            approval_timeout_secs: 300,
            reply_in_thread: true,
            ack_reactions: true,
        });
        let entries = all_integrations(&config);
        let mx = entries.iter().find(|e| e.name == "Matrix").unwrap();
        assert!(matches!(mx.status, IntegrationStatus::Active));
    }

    #[test]
    fn cron_active_when_enabled() {
        let mut config = Config::default();
        config.cron.enabled = true;
        let entries = all_integrations(&config);
        let cron = entries.iter().find(|e| e.name == "Cron").unwrap();
        assert!(matches!(cron.status, IntegrationStatus::Active));
    }

    #[test]
    fn browser_active_when_enabled() {
        let mut config = Config::default();
        config.browser.enabled = true;
        let entries = all_integrations(&config);
        let browser = entries.iter().find(|e| e.name == "Browser").unwrap();
        assert!(matches!(browser.status, IntegrationStatus::Active));
    }

    #[test]
    fn shell_filesystem_weather_always_active() {
        let config = Config::default();
        let entries = all_integrations(&config);
        for name in ["Shell", "File System", "Weather"] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert!(
                matches!(entry.status, IntegrationStatus::Active),
                "{name} should always be Active"
            );
        }
    }

    #[test]
    fn macos_active_on_macos() {
        let config = Config::default();
        let entries = all_integrations(&config);
        let macos = entries.iter().find(|e| e.name == "macOS").unwrap();
        if cfg!(target_os = "macos") {
            assert!(matches!(macos.status, IntegrationStatus::Active));
        } else {
            assert!(matches!(macos.status, IntegrationStatus::Available));
        }
    }

    #[test]
    fn channel_list_derives_from_schema_includes_previously_missing_channels() {
        // The hand-maintained list in master only surfaced ~11 channels;
        // ChannelsConfig actually declares ~25+. This test pins the
        // schema-derived path so adding a new channel surfaces here.
        let config = Config::default();
        let entries = all_integrations(&config);
        let chat_names: std::collections::HashSet<&str> = entries
            .iter()
            .filter(|e| e.category == IntegrationCategory::Chat)
            .map(|e| e.name.as_str())
            .collect();
        for must in [
            "Telegram",
            "Discord",
            "Slack",
            "Matrix",
            "iMessage",
            "WhatsApp",
            // Previously missing from the hand-list:
            "Mattermost",
            "IRC",
            "Lark",
            "Line",
            "Feishu",
            "WeCom",
            "WeChat",
            "Reddit",
            "Bluesky",
            "MQTT",
            "Discord History",
        ] {
            assert!(
                chat_names.contains(must),
                "Chat category should include {must} (derived from schema)"
            );
        }
    }

    #[test]
    fn regional_provider_aliases_activate_expected_ai_integrations() {
        let mut config = Config::default();

        config.providers.fallback = Some("minimax-cn".to_string());
        let entries = all_integrations(&config);
        let minimax = entries.iter().find(|e| e.name == "MiniMax").unwrap();
        assert!(matches!(minimax.status, IntegrationStatus::Active));

        config.providers.fallback = Some("glm-cn".to_string());
        let entries = all_integrations(&config);
        let glm = entries.iter().find(|e| e.name == "GLM").unwrap();
        assert!(matches!(glm.status, IntegrationStatus::Active));

        config.providers.fallback = Some("moonshot-intl".to_string());
        let entries = all_integrations(&config);
        let moonshot = entries.iter().find(|e| e.name == "Moonshot").unwrap();
        assert!(matches!(moonshot.status, IntegrationStatus::Active));

        config.providers.fallback = Some("qwen-intl".to_string());
        let entries = all_integrations(&config);
        let qwen = entries.iter().find(|e| e.name == "Qwen").unwrap();
        assert!(matches!(qwen.status, IntegrationStatus::Active));

        config.providers.fallback = Some("zai-cn".to_string());
        let entries = all_integrations(&config);
        let zai = entries.iter().find(|e| e.name == "Z.AI").unwrap();
        assert!(matches!(zai.status, IntegrationStatus::Active));

        config.providers.fallback = Some("baidu".to_string());
        let entries = all_integrations(&config);
        let qianfan = entries.iter().find(|e| e.name == "Qianfan").unwrap();
        assert!(matches!(qianfan.status, IntegrationStatus::Active));
    }
}
