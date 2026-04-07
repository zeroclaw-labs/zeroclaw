use super::{IntegrationCategory, IntegrationEntry, IntegrationStatus};

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
        // ── AI Models ───────────────────────────────────────────
        IntegrationEntry {
            name: "OpenRouter",
            description: "200+ models, 1 API key",
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
            description: "Claude 3.5/4 Sonnet & Opus",
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
            description: "GPT-4o, GPT-5, o1",
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
            name: "Google Gemini",
            description: "Gemini 2.5 Pro/Flash",
            category: IntegrationCategory::AiModel,
            status_fn: |c| {
                if matches!(
                    c.default_provider.as_deref(),
                    Some("gemini" | "google" | "google-gemini")
                ) {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
        // ── Productivity ────────────────────────────────────────
        IntegrationEntry {
            name: "Google Workspace",
            description: "Drive, Gmail, Calendar, Sheets, Docs via gws CLI",
            category: IntegrationCategory::Productivity,
            status_fn: |c| {
                if c.google_workspace.enabled {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
        },
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
            status_fn: |c| {
                if c.browser.enabled {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
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
            status_fn: |c| {
                if c.cron.enabled {
                    IntegrationStatus::Active
                } else {
                    IntegrationStatus::Available
                }
            },
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
            status_fn: |_| IntegrationStatus::Active,
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
            description: "Chat via Telegram/Slack",
            category: IntegrationCategory::Platform,
            status_fn: |_| IntegrationStatus::Available,
        },
        IntegrationEntry {
            name: "Android",
            description: "Chat via Telegram/Slack",
            category: IntegrationCategory::Platform,
            status_fn: |_| IntegrationStatus::Available,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::config::schema::{StreamMode, TelegramConfig};

    #[test]
    fn registry_has_entries() {
        let entries = all_integrations();
        assert!(
            entries.len() >= 40,
            "Expected 40+ integrations, got {}",
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
            ack_reactions: None,
            proxy_url: None,
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
    fn coming_soon_integrations_stay_coming_soon() {
        let config = Config::default();
        let entries = all_integrations();
        for name in ["Spotify", "Home Assistant"] {
            let entry = entries.iter().find(|e| e.name == name).unwrap();
            assert!(
                matches!((entry.status_fn)(&config), IntegrationStatus::ComingSoon),
                "{name} should be ComingSoon"
            );
        }
    }

    #[test]
    fn cron_active_when_enabled() {
        let mut config = Config::default();
        config.cron.enabled = true;
        let entries = all_integrations();
        let cron = entries.iter().find(|e| e.name == "Cron").unwrap();
        assert!(matches!(
            (cron.status_fn)(&config),
            IntegrationStatus::Active
        ));
    }

    #[test]
    fn cron_available_when_disabled() {
        let mut config = Config::default();
        config.cron.enabled = false;
        let entries = all_integrations();
        let cron = entries.iter().find(|e| e.name == "Cron").unwrap();
        assert!(matches!(
            (cron.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn browser_active_when_enabled() {
        let mut config = Config::default();
        config.browser.enabled = true;
        let entries = all_integrations();
        let browser = entries.iter().find(|e| e.name == "Browser").unwrap();
        assert!(matches!(
            (browser.status_fn)(&config),
            IntegrationStatus::Active
        ));
    }

    #[test]
    fn browser_available_when_disabled() {
        let mut config = Config::default();
        config.browser.enabled = false;
        let entries = all_integrations();
        let browser = entries.iter().find(|e| e.name == "Browser").unwrap();
        assert!(matches!(
            (browser.status_fn)(&config),
            IntegrationStatus::Available
        ));
    }

    #[test]
    fn shell_and_filesystem_always_active() {
        let config = Config::default();
        let entries = all_integrations();
        for name in ["Shell", "File System", "Weather"] {
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
            ai_count >= 4,
            "Expected 4+ AI model integrations, got {ai_count}"
        );
    }
}
