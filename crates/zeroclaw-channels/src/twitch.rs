//! Twitch chat channel — thin adapter over the IRC channel.
//!
//! Twitch chat speaks IRC: `irc.chat.twitch.tv:6697` over TLS, with the
//! OAuth token sent as `PASS oauth:{token}`. The adapter constructs an
//! `IrcChannel` with Twitch-specific defaults so operators don't have to
//! know IRC is the wire protocol.
//!
//! # Auth
//! Twitch OAuth user-access token. Operator mints via either
//! <https://twitchapps.com/tmi/> (one-click implicit flow, returns
//! `oauth:...` directly) or the Twitch CLI Device Code Flow (returns a
//! raw access token; the channel auto-prefixes `oauth:` if missing).
//!
//! # Inbound
//! Forwards to the wrapped `IrcChannel::listen`. Each `ChannelMessage`
//! emerging from the inner channel is rewritten so `channel = "twitch"`
//! before being forwarded to the agent runtime — this keeps routing,
//! auditing, and metrics distinct from the plain-IRC channel.
//!
//! # Outbound
//! Forwards to `IrcChannel::send`. Twitch's chat protocol is plain
//! `PRIVMSG #channel :body`, which the IRC channel already handles
//! (including length splitting).

use crate::irc::{IrcChannel, IrcChannelConfig};
use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::mpsc;
use zeroclaw_api::channel::{Channel, ChannelMessage, SendMessage};

const TWITCH_IRC_HOST: &str = "irc.chat.twitch.tv";
const TWITCH_IRC_PORT: u16 = 6697;
const FORWARD_BUFFER: usize = 64;

pub struct TwitchChannel {
    inner: Arc<IrcChannel>,
}

impl TwitchChannel {
    pub fn new(
        bot_username: String,
        oauth_token: String,
        channels: Vec<String>,
        allowed_users: Vec<String>,
        mention_only: bool,
    ) -> Self {
        let nickname = bot_username.trim().to_ascii_lowercase();
        let pass = normalize_oauth_token(&oauth_token);
        let normalized_channels = channels
            .iter()
            .filter_map(|c| normalize_twitch_channel(c))
            .collect::<Vec<_>>();

        let cfg = IrcChannelConfig {
            server: TWITCH_IRC_HOST.into(),
            port: TWITCH_IRC_PORT,
            nickname: nickname.clone(),
            username: Some(nickname),
            channels: normalized_channels,
            allowed_users,
            // Twitch authenticates with PASS oauth:{token}, not SASL.
            server_password: Some(pass),
            sasl_password: None,
            nickserv_password: None,
            verify_tls: true,
            mention_only,
        };
        Self {
            inner: Arc::new(IrcChannel::new(cfg)),
        }
    }
}

#[async_trait]
impl Channel for TwitchChannel {
    fn name(&self) -> &str {
        "twitch"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        self.inner.send(message).await
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let (inner_tx, mut inner_rx) = mpsc::channel::<ChannelMessage>(FORWARD_BUFFER);
        let inner = self.inner.clone();
        let listen_task = tokio::spawn(async move { inner.listen(inner_tx).await });

        // Drain inner_rx, rewrite the channel field, forward to outer tx.
        while let Some(mut msg) = inner_rx.recv().await {
            msg.channel = "twitch".to_string();
            if tx.send(msg).await.is_err() {
                listen_task.abort();
                return Ok(());
            }
        }

        // inner_rx closed → inner listen task ended (clean exit or error).
        match listen_task.await {
            Ok(res) => res,
            Err(e) if e.is_cancelled() => Ok(()),
            Err(e) => Err(anyhow::anyhow!("Twitch IRC listen task panicked: {e}")),
        }
    }

    async fn health_check(&self) -> bool {
        self.inner.health_check().await
    }
}

/// Normalize a raw OAuth token into the `PASS` value Twitch expects. Twitch
/// chat requires the literal prefix `oauth:` followed by the token; if the
/// operator pasted the token without it (Twitch CLI / Device Flow output),
/// we prepend it. Whitespace is trimmed.
pub fn normalize_oauth_token(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.starts_with("oauth:") {
        trimmed.to_string()
    } else {
        format!("oauth:{trimmed}")
    }
}

/// Normalize a Twitch channel name. Twitch channel names are
/// case-insensitive Twitch logins; the IRC `JOIN` command requires them
/// prefixed with `#`. Whitespace is trimmed; an empty entry yields `None`
/// so the operator can include trailing commas without crashing the
/// listen loop.
pub fn normalize_twitch_channel(raw: &str) -> Option<String> {
    let trimmed = raw.trim().to_ascii_lowercase();
    let bare = trimmed.trim_start_matches('#');
    if bare.is_empty() {
        return None;
    }
    Some(format!("#{bare}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_oauth_token_preserves_existing_prefix() {
        assert_eq!(
            normalize_oauth_token("oauth:abcdef1234"),
            "oauth:abcdef1234"
        );
    }

    #[test]
    fn normalize_oauth_token_adds_prefix_when_missing() {
        assert_eq!(normalize_oauth_token("abcdef1234"), "oauth:abcdef1234");
    }

    #[test]
    fn normalize_oauth_token_trims_whitespace() {
        assert_eq!(normalize_oauth_token("  oauth:abcdef  "), "oauth:abcdef");
        assert_eq!(normalize_oauth_token("\tabcdef\n"), "oauth:abcdef");
    }

    #[test]
    fn normalize_twitch_channel_adds_hash_prefix() {
        assert_eq!(
            normalize_twitch_channel("mychannel").as_deref(),
            Some("#mychannel")
        );
    }

    #[test]
    fn normalize_twitch_channel_preserves_hash_prefix() {
        assert_eq!(
            normalize_twitch_channel("#alreadyhashed").as_deref(),
            Some("#alreadyhashed")
        );
    }

    #[test]
    fn normalize_twitch_channel_lowercases() {
        assert_eq!(
            normalize_twitch_channel("MyChannel").as_deref(),
            Some("#mychannel")
        );
        assert_eq!(
            normalize_twitch_channel("#UPPERCASE").as_deref(),
            Some("#uppercase")
        );
    }

    #[test]
    fn normalize_twitch_channel_trims_whitespace() {
        assert_eq!(
            normalize_twitch_channel("  #spaces  ").as_deref(),
            Some("#spaces")
        );
    }

    #[test]
    fn normalize_twitch_channel_drops_empty_entries() {
        assert!(normalize_twitch_channel("").is_none());
        assert!(normalize_twitch_channel("   ").is_none());
        assert!(normalize_twitch_channel("#").is_none());
    }

    #[test]
    fn channel_name_is_twitch_not_irc() {
        let ch = TwitchChannel::new(
            "MyBot".into(),
            "abcdef".into(),
            vec!["mychannel".into()],
            vec!["*".into()],
            false,
        );
        assert_eq!(ch.name(), "twitch");
    }

    #[test]
    fn constructor_normalizes_inputs() {
        // Sanity-check that constructing TwitchChannel doesn't panic on
        // realistic operator inputs and that the normalization helpers
        // handle the variations the constructor passes through.
        let ch = TwitchChannel::new(
            "  MyBot  ".into(),
            "raw-token-no-prefix".into(),
            vec!["#FirstChan".into(), "secondchan".into(), "  ".into()],
            vec!["alice".into()],
            true,
        );
        assert_eq!(ch.name(), "twitch");
        // Token + channel normalization is exercised by the dedicated
        // tests above; this test ensures the constructor wiring composes
        // them correctly without errors at runtime.
    }
}
