//! Shared channel metadata used by onboarding and channel UX surfaces.

/// Lightweight metadata for onboarding channel selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnboardingChannelOption {
    pub id: &'static str,
    pub display_name: &'static str,
    pub hint: &'static str,
    pub installed: bool,
}

/// Canonical onboarding channel list (single source of truth).
pub const ONBOARDING_CHANNELS: &[OnboardingChannelOption] = &[
    OnboardingChannelOption {
        id: "telegram",
        display_name: "Telegram",
        hint: "Bot API",
        installed: false,
    },
    OnboardingChannelOption {
        id: "whatsapp",
        display_name: "WhatsApp",
        hint: "QR link",
        installed: true,
    },
    OnboardingChannelOption {
        id: "discord",
        display_name: "Discord",
        hint: "Bot API",
        installed: false,
    },
    OnboardingChannelOption {
        id: "irc",
        display_name: "IRC",
        hint: "Server + Nick",
        installed: false,
    },
    OnboardingChannelOption {
        id: "google_chat",
        display_name: "Google Chat",
        hint: "Chat API",
        installed: true,
    },
    OnboardingChannelOption {
        id: "slack",
        display_name: "Slack",
        hint: "Socket Mode",
        installed: false,
    },
    OnboardingChannelOption {
        id: "signal",
        display_name: "Signal",
        hint: "signal-cli",
        installed: false,
    },
    OnboardingChannelOption {
        id: "imessage",
        display_name: "iMessage",
        hint: "imsg",
        installed: false,
    },
    OnboardingChannelOption {
        id: "line",
        display_name: "LINE",
        hint: "Messaging API",
        installed: false,
    },
    OnboardingChannelOption {
        id: "mattermost",
        display_name: "Mattermost",
        hint: "plugin",
        installed: false,
    },
    OnboardingChannelOption {
        id: "nextcloud_talk",
        display_name: "Nextcloud Talk",
        hint: "self-hosted",
        installed: false,
    },
    OnboardingChannelOption {
        id: "feishu_lark",
        display_name: "Feishu/Lark",
        hint: "飞书",
        installed: false,
    },
    OnboardingChannelOption {
        id: "bluebubbles",
        display_name: "BlueBubbles",
        hint: "macOS app",
        installed: false,
    },
    OnboardingChannelOption {
        id: "zalo",
        display_name: "Zalo",
        hint: "Bot API",
        installed: false,
    },
    OnboardingChannelOption {
        id: "synology_chat",
        display_name: "Synology Chat",
        hint: "Webhook",
        installed: false,
    },
    OnboardingChannelOption {
        id: "nostr",
        display_name: "Nostr",
        hint: "NIP-04 DMs",
        installed: true,
    },
    OnboardingChannelOption {
        id: "microsoft_teams",
        display_name: "Microsoft Teams",
        hint: "Teams SDK",
        installed: true,
    },
    OnboardingChannelOption {
        id: "matrix",
        display_name: "Matrix",
        hint: "plugin",
        installed: true,
    },
    OnboardingChannelOption {
        id: "zalo_personal",
        display_name: "Zalo Personal",
        hint: "Personal Account",
        installed: true,
    },
    OnboardingChannelOption {
        id: "tlon",
        display_name: "Tlon",
        hint: "Urbit",
        installed: true,
    },
    OnboardingChannelOption {
        id: "twitch",
        display_name: "Twitch",
        hint: "Chat",
        installed: true,
    },
    OnboardingChannelOption {
        id: "skip",
        display_name: "Skip for now",
        hint: "configure later",
        installed: false,
    },
];
