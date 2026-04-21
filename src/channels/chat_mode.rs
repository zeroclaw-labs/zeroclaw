//! Chat mode framework — observer vs. participant operating modes for channels.
//!
//! ZeroClaw can interact with messaging channels in two distinct modes:
//!
//! - **Participant**: the bot is a full member of the chat/group. It reads
//!   messages directly from the platform and posts replies directly. This is
//!   the historical default for Telegram, Discord, Slack, Matrix, etc.
//! - **Observer**: the bot is *not* in the target group. The user manually
//!   forwards relevant messages to the bot's 1:1 chat, and the bot replies
//!   in the 1:1 chat with a one-tap "share back to group" button that uses
//!   the platform's native share/forward UI. KakaoTalk operates in this
//!   mode by necessity (no official API access to third-party group chats).
//!
//! Each [`Channel`](crate::channels::traits::Channel) declares which modes it
//! supports via [`Channel::supported_chat_modes`]. The user can switch
//! between supported modes per channel via the `/mode` command. The active
//! mode is held in [`ChatModeStore`] keyed by `(channel, platform_uid)`.
//!
//! ## v1 wiring scope
//!
//! KakaoTalk: observer-only, fully wired (the only mode it supports).
//! Other channels: declare both modes supported but only participant mode
//! is fully wired in v1. `/mode observer` on those channels returns an
//! "지원 예정" message until the per-channel observer UX lands in a
//! follow-up PR. This keeps the mode abstraction in place without shipping
//! half-implemented observer paths across eight channels at once.

use parking_lot::RwLock;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Operating mode for a channel/user pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatMode {
    /// Bot is a full member of the chat — reads and posts directly.
    Participant,
    /// Bot is not in the chat — relies on user-driven forward in and
    /// share-back-button forward out.
    Observer,
}

impl ChatMode {
    /// Slug used in the `/mode <slug>` command and stored in memory tags.
    pub fn as_slug(self) -> &'static str {
        match self {
            Self::Participant => "participant",
            Self::Observer => "observer",
        }
    }

    /// Parse the `/mode` argument. Accepts both English slugs and Korean
    /// labels so users can type either.
    pub fn parse_user_input(input: &str) -> Option<Self> {
        let normalized = input.trim().to_lowercase();
        match normalized.as_str() {
            "participant" | "참가자" | "참가" | "join" | "joined" => Some(Self::Participant),
            "observer" | "옵저버" | "관전" | "관찰" | "watch" => Some(Self::Observer),
            _ => None,
        }
    }

    /// Korean display label for chat replies.
    pub fn display_label_ko(self) -> &'static str {
        match self {
            Self::Participant => "참가자 모드",
            Self::Observer => "옵저버 모드",
        }
    }
}

impl fmt::Display for ChatMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_slug())
    }
}

/// Per-(channel, platform_uid) active-mode registry. In-memory only; the
/// active mode is intentionally ephemeral — defaults are applied on each
/// process start, and the user can re-issue `/mode` whenever needed.
///
/// Persistence of mode choice across restarts is deliberately deferred:
/// when an `auth_store` link is present, future work can hang the saved
/// mode off `ChannelLink`. v1 keeps the in-memory store to avoid
/// schema churn (CLAUDE.md §3.3 rule-of-three: only one consumer today).
#[derive(Debug, Default)]
pub struct ChatModeStore {
    inner: RwLock<HashMap<(String, String), ChatMode>>,
}

impl ChatModeStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Look up the active mode. Returns `None` if the user has not chosen
    /// a mode — callers should fall back to `default_chat_mode_for(channel)`.
    pub fn current(&self, channel: &str, platform_uid: &str) -> Option<ChatMode> {
        let guard = self.inner.read();
        guard
            .get(&(channel.to_string(), platform_uid.to_string()))
            .copied()
    }

    /// Set the active mode for a `(channel, platform_uid)` pair.
    pub fn set(&self, channel: &str, platform_uid: &str, mode: ChatMode) {
        let mut guard = self.inner.write();
        guard.insert((channel.to_string(), platform_uid.to_string()), mode);
    }

    /// Clear the override; next `current()` will return `None`.
    pub fn clear(&self, channel: &str, platform_uid: &str) {
        let mut guard = self.inner.write();
        guard.remove(&(channel.to_string(), platform_uid.to_string()));
    }

    /// Resolve the effective mode: explicit override → channel default.
    pub fn effective(&self, channel: &str, platform_uid: &str) -> ChatMode {
        self.current(channel, platform_uid)
            .unwrap_or_else(|| default_chat_mode_for(channel))
    }
}

/// Default mode for a channel when the user has not made an explicit choice.
///
/// KakaoTalk defaults to (and only supports) observer mode. All other
/// channels default to participant mode — that matches their historical
/// behavior so this PR introduces no behavior change for them.
pub fn default_chat_mode_for(channel: &str) -> ChatMode {
    match channel {
        "kakao" => ChatMode::Observer,
        _ => ChatMode::Participant,
    }
}

/// Whether `mode` is fully wired for `channel` in v1. Channels can declare
/// support for a mode via [`Channel::supported_chat_modes`](crate::channels::traits::Channel::supported_chat_modes)
/// while the actual UX wiring lands in a later PR; this flag lets the
/// `/mode` command tell the user "지원 예정" instead of silently switching
/// to a half-implemented mode.
///
/// v1 contract: kakao/observer is wired. participant on every other channel
/// is wired (current default behavior). Observer on non-kakao channels is
/// declared-but-not-wired.
//
// The four match arms are intentionally kept explicit even though two pairs
// share bodies — each arm documents one cell of the v1 capability matrix.
// Collapsing would hide the matrix for a trivial deduplication win.
#[allow(clippy::match_same_arms)]
pub fn is_mode_wired_v1(channel: &str, mode: ChatMode) -> bool {
    match (channel, mode) {
        ("kakao", ChatMode::Observer) => true,
        ("kakao", ChatMode::Participant) => false,
        (_, ChatMode::Participant) => true,
        (_, ChatMode::Observer) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_user_input_accepts_korean_and_english() {
        assert_eq!(
            ChatMode::parse_user_input("observer"),
            Some(ChatMode::Observer)
        );
        assert_eq!(
            ChatMode::parse_user_input("옵저버"),
            Some(ChatMode::Observer)
        );
        assert_eq!(ChatMode::parse_user_input("관전"), Some(ChatMode::Observer));
        assert_eq!(
            ChatMode::parse_user_input("participant"),
            Some(ChatMode::Participant)
        );
        assert_eq!(
            ChatMode::parse_user_input("참가자"),
            Some(ChatMode::Participant)
        );
        assert_eq!(ChatMode::parse_user_input("nonsense"), None);
    }

    #[test]
    fn parse_user_input_normalizes_whitespace_and_case() {
        assert_eq!(
            ChatMode::parse_user_input("  OBSERVER  "),
            Some(ChatMode::Observer)
        );
    }

    #[test]
    fn store_returns_none_until_set() {
        let store = ChatModeStore::new();
        assert!(store.current("telegram", "u_1").is_none());
        store.set("telegram", "u_1", ChatMode::Observer);
        assert_eq!(store.current("telegram", "u_1"), Some(ChatMode::Observer));
    }

    #[test]
    fn store_isolates_users_and_channels() {
        let store = ChatModeStore::new();
        store.set("telegram", "u_1", ChatMode::Observer);
        store.set("discord", "u_1", ChatMode::Participant);
        store.set("telegram", "u_2", ChatMode::Participant);
        assert_eq!(store.current("telegram", "u_1"), Some(ChatMode::Observer));
        assert_eq!(store.current("discord", "u_1"), Some(ChatMode::Participant));
        assert_eq!(
            store.current("telegram", "u_2"),
            Some(ChatMode::Participant)
        );
    }

    #[test]
    fn clear_removes_override() {
        let store = ChatModeStore::new();
        store.set("kakao", "u_1", ChatMode::Observer);
        store.clear("kakao", "u_1");
        assert!(store.current("kakao", "u_1").is_none());
    }

    #[test]
    fn effective_falls_back_to_channel_default() {
        let store = ChatModeStore::new();
        assert_eq!(store.effective("kakao", "u_x"), ChatMode::Observer);
        assert_eq!(store.effective("telegram", "u_x"), ChatMode::Participant);
        assert_eq!(store.effective("unknown", "u_x"), ChatMode::Participant);
    }

    #[test]
    fn effective_respects_explicit_override() {
        let store = ChatModeStore::new();
        store.set("telegram", "u_1", ChatMode::Observer);
        assert_eq!(store.effective("telegram", "u_1"), ChatMode::Observer);
    }

    #[test]
    fn default_chat_mode_for_known_channels() {
        assert_eq!(default_chat_mode_for("kakao"), ChatMode::Observer);
        assert_eq!(default_chat_mode_for("telegram"), ChatMode::Participant);
        assert_eq!(default_chat_mode_for("discord"), ChatMode::Participant);
        assert_eq!(default_chat_mode_for("slack"), ChatMode::Participant);
    }

    #[test]
    fn v1_wiring_kakao_observer_only() {
        assert!(is_mode_wired_v1("kakao", ChatMode::Observer));
        assert!(!is_mode_wired_v1("kakao", ChatMode::Participant));
    }

    #[test]
    fn v1_wiring_other_channels_participant_only() {
        assert!(is_mode_wired_v1("telegram", ChatMode::Participant));
        assert!(!is_mode_wired_v1("telegram", ChatMode::Observer));
        assert!(is_mode_wired_v1("discord", ChatMode::Participant));
        assert!(!is_mode_wired_v1("discord", ChatMode::Observer));
    }

    #[test]
    fn slug_round_trip() {
        for mode in [ChatMode::Observer, ChatMode::Participant] {
            assert_eq!(ChatMode::parse_user_input(mode.as_slug()), Some(mode));
        }
    }
}
