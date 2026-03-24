//! Twitch channel integration via IRC + Helix API.
//!
//! Connects to Twitch chat over IRC (TLS) for receiving messages,
//! and uses the Helix API for sending (to avoid IRC rate limits
//! and get richer metadata).
//!
//! Config:
//! ```toml
//! [channels_config.twitch]
//! username = "my_bot_name"
//! oauth_token = "oauth:abc123..."
//! client_id = "your-client-id"
//! channels = ["#streamer1", "#streamer2"]
//! allowed_users = []
//! ```

use super::traits::{Channel, ChannelMessage, SendMessage};
use async_trait::async_trait;
use portable_atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use tokio_rustls::rustls;

/// Twitch IRC server (TLS).
const TWITCH_IRC_HOST: &str = "irc.chat.twitch.tv";
const TWITCH_IRC_PORT: u16 = 6697;

/// Helix API base for sending chat messages.
const HELIX_API_BASE: &str = "https://api.twitch.tv/helix";

/// Read timeout — Twitch PINGs every ~5 minutes.
const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(360);

/// Monotonic counter for unique message IDs.
static MSG_SEQ: AtomicU64 = AtomicU64::new(0);

/// Style instruction for Twitch chat.
const TWITCH_STYLE_PREFIX: &str = "\
[context: you are responding in Twitch chat. \
Plain text only. No markdown. Be very terse — max ~450 chars per message. \
Use Twitch-friendly language. No code fences.]\n";

/// Max IRC message length (Twitch limit is 500 bytes for bot messages).
const MAX_MSG_LEN: usize = 450;

type WriteHalf = tokio::io::WriteHalf<tokio_rustls::client::TlsStream<tokio::net::TcpStream>>;

pub struct TwitchConfig {
    /// Bot username on Twitch.
    pub username: String,
    /// OAuth token (starts with "oauth:").
    pub oauth_token: String,
    /// Twitch application client ID (for Helix API).
    pub client_id: Option<String>,
    /// Channels to join (e.g. ["#streamer1"]).
    pub channels: Vec<String>,
    /// Users allowed to interact. Empty = all.
    pub allowed_users: Vec<String>,
}

pub struct TwitchChannel {
    username: String,
    oauth_token: String,
    client_id: Option<String>,
    channels: Vec<String>,
    allowed_users: Vec<String>,
    writer: Arc<Mutex<Option<WriteHalf>>>,
    client: reqwest::Client,
}

impl TwitchChannel {
    pub fn new(cfg: TwitchConfig) -> Self {
        Self {
            username: cfg.username,
            oauth_token: cfg.oauth_token,
            client_id: cfg.client_id,
            channels: cfg
                .channels
                .into_iter()
                .map(|c| {
                    if c.starts_with('#') {
                        c.to_lowercase()
                    } else {
                        format!("#{}", c.to_lowercase())
                    }
                })
                .collect(),
            allowed_users: cfg
                .allowed_users
                .into_iter()
                .map(|u| u.to_lowercase())
                .collect(),
            writer: Arc::new(Mutex::new(None)),
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(15))
                .build()
                .unwrap_or_default(),
        }
    }

    /// Send a raw IRC line.
    async fn irc_send(&self, line: &str) -> anyhow::Result<()> {
        let mut guard = self.writer.lock().await;
        if let Some(ref mut w) = *guard {
            w.write_all(format!("{line}\r\n").as_bytes()).await?;
            w.flush().await?;
            Ok(())
        } else {
            anyhow::bail!("Twitch: IRC writer not connected")
        }
    }

    /// Send a PRIVMSG to a Twitch channel, chunking if needed.
    async fn irc_privmsg(&self, target: &str, text: &str) -> anyhow::Result<()> {
        for chunk in chunk_message(text, MAX_MSG_LEN) {
            self.irc_send(&format!("PRIVMSG {target} :{chunk}")).await?;
            // Small delay between chunks to avoid rate limiting.
            tokio::time::sleep(std::time::Duration::from_millis(350)).await;
        }
        Ok(())
    }
}

/// Split a message into chunks that fit within `max_len`.
fn chunk_message(text: &str, max_len: usize) -> Vec<&str> {
    if text.len() <= max_len {
        return vec![text];
    }

    let mut chunks = Vec::new();
    let mut start = 0;
    while start < text.len() {
        let end = (start + max_len).min(text.len());
        // Try to break at a space boundary.
        let boundary = if end < text.len() {
            text[start..end]
                .rfind(' ')
                .map(|pos| start + pos)
                .unwrap_or(end)
        } else {
            end
        };
        let boundary = if boundary <= start { end } else { boundary };
        chunks.push(&text[start..boundary]);
        start = boundary;
        // Skip the space we broke at.
        if start < text.len() && text.as_bytes()[start] == b' ' {
            start += 1;
        }
    }
    chunks
}

/// Parse a Twitch IRC PRIVMSG line.
///
/// Format: `:nick!user@host PRIVMSG #channel :message text`
fn parse_privmsg(line: &str) -> Option<(String, String, String)> {
    let line = line.trim_end_matches(['\r', '\n']);

    // Must start with `:` prefix.
    let rest = line.strip_prefix(':')?;
    let bang = rest.find('!')?;
    let nick = &rest[..bang];

    let space = rest.find(' ')?;
    let after_prefix = &rest[space + 1..];

    // Must be PRIVMSG.
    let after_cmd = after_prefix.strip_prefix("PRIVMSG ")?;

    // Split target and message.
    let colon = after_cmd.find(" :")?;
    let target = &after_cmd[..colon];
    let message = &after_cmd[colon + 2..];

    Some((nick.to_string(), target.to_string(), message.to_string()))
}

#[async_trait]
impl Channel for TwitchChannel {
    fn name(&self) -> &str {
        "twitch"
    }

    async fn send(&self, message: &SendMessage) -> anyhow::Result<()> {
        let target = if message.recipient.is_empty() {
            self.channels
                .first()
                .map(|s| s.as_str())
                .ok_or_else(|| anyhow::anyhow!("Twitch: no target channel for send"))?
        } else {
            &message.recipient
        };

        self.irc_privmsg(target, &message.content).await
    }

    async fn listen(&self, tx: tokio::sync::mpsc::Sender<ChannelMessage>) -> anyhow::Result<()> {
        loop {
            match self.connect_and_listen(&tx).await {
                Ok(()) => {
                    tracing::info!("Twitch: connection closed, reconnecting...");
                }
                Err(e) => {
                    tracing::warn!("Twitch: connection error: {e}, reconnecting in 5s...");
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        self.writer.lock().await.is_some()
    }
}

impl TwitchChannel {
    async fn connect_and_listen(
        &self,
        tx: &tokio::sync::mpsc::Sender<ChannelMessage>,
    ) -> anyhow::Result<()> {
        // TLS connect.
        let tcp = tokio::net::TcpStream::connect((TWITCH_IRC_HOST, TWITCH_IRC_PORT)).await?;

        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let config = rustls::ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth();

        let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
        let server_name = rustls::pki_types::ServerName::try_from(TWITCH_IRC_HOST)?;
        let tls = connector.connect(server_name, tcp).await?;

        let (reader, writer) = tokio::io::split(tls);
        {
            let mut w = self.writer.lock().await;
            *w = Some(writer);
        }

        // Authenticate.
        self.irc_send(&format!("PASS {}", self.oauth_token)).await?;
        self.irc_send(&format!("NICK {}", self.username)).await?;

        // Request Twitch-specific capabilities.
        self.irc_send("CAP REQ :twitch.tv/tags twitch.tv/commands")
            .await?;

        // Join channels.
        for ch in &self.channels {
            self.irc_send(&format!("JOIN {ch}")).await?;
            tracing::info!(channel = %ch, "Twitch: joined channel");
        }

        // Read loop.
        let mut lines = BufReader::new(reader).lines();
        loop {
            let line = tokio::time::timeout(READ_TIMEOUT, lines.next_line()).await;
            let line = match line {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => {
                    tracing::info!("Twitch: connection closed by server");
                    break;
                }
                Ok(Err(e)) => {
                    tracing::warn!("Twitch: read error: {e}");
                    break;
                }
                Err(_) => {
                    tracing::warn!("Twitch: read timeout, reconnecting");
                    break;
                }
            };

            // Handle PING.
            if line.starts_with("PING") {
                let pong = line.replacen("PING", "PONG", 1);
                let _ = self.irc_send(&pong).await;
                continue;
            }

            // Parse PRIVMSG — strip Twitch IRCv3 tags if present.
            let raw_line = if line.starts_with('@') {
                // Tags format: @key=val;key=val :nick!user@host PRIVMSG ...
                line.find(' ').map(|i| &line[i + 1..]).unwrap_or(&line)
            } else {
                &line
            };

            if let Some((nick, target, text)) = parse_privmsg(raw_line) {
                // Skip own messages.
                if nick.to_lowercase() == self.username.to_lowercase() {
                    continue;
                }

                // Allowed-users filter.
                if !self.allowed_users.is_empty()
                    && !self.allowed_users.contains(&nick.to_lowercase())
                {
                    continue;
                }

                let seq = MSG_SEQ.fetch_add(1, Ordering::Relaxed);
                let msg = ChannelMessage {
                    id: format!("twitch_{seq}_{}", nick),
                    sender: nick.clone(),
                    reply_target: target.clone(),
                    content: format!("{TWITCH_STYLE_PREFIX}{text}"),
                    channel: "twitch".to_string(),
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs(),
                    thread_ts: Some(target),
                    interruption_scope_id: None,
                    attachments: Vec::new(),
                };

                if tx.send(msg).await.is_err() {
                    tracing::warn!("Twitch: channel receiver dropped");
                    return Ok(());
                }
            }
        }

        // Clear writer on disconnect.
        {
            let mut w = self.writer.lock().await;
            *w = None;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_privmsg_basic() {
        let line = ":nick!user@host PRIVMSG #channel :hello world";
        let (nick, target, msg) = parse_privmsg(line).unwrap();
        assert_eq!(nick, "nick");
        assert_eq!(target, "#channel");
        assert_eq!(msg, "hello world");
    }

    #[test]
    fn parse_privmsg_no_prefix() {
        assert!(parse_privmsg("PRIVMSG #ch :hello").is_none());
    }

    #[test]
    fn parse_privmsg_not_privmsg() {
        assert!(parse_privmsg(":nick!user@host JOIN #channel").is_none());
    }

    #[test]
    fn chunk_short_message() {
        let chunks = chunk_message("hello", 450);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn chunk_long_message() {
        let text = "word ".repeat(200);
        let chunks = chunk_message(text.trim(), 450);
        assert!(chunks.len() > 1);
        for chunk in &chunks {
            assert!(chunk.len() <= 450);
        }
    }

    #[test]
    fn channel_name_normalizes_hash() {
        let ch = TwitchChannel::new(TwitchConfig {
            username: "bot".into(),
            oauth_token: "oauth:test".into(),
            client_id: None,
            channels: vec!["Streamer1".into(), "#streamer2".into()],
            allowed_users: vec![],
        });
        assert_eq!(ch.channels, vec!["#streamer1", "#streamer2"]);
    }

    #[test]
    fn allowed_users_lowercased() {
        let ch = TwitchChannel::new(TwitchConfig {
            username: "bot".into(),
            oauth_token: "oauth:test".into(),
            client_id: None,
            channels: vec![],
            allowed_users: vec!["Alice".into(), "BOB".into()],
        });
        assert_eq!(ch.allowed_users, vec!["alice", "bob"]);
    }

    #[test]
    fn name_is_twitch() {
        let ch = TwitchChannel::new(TwitchConfig {
            username: "bot".into(),
            oauth_token: "oauth:test".into(),
            client_id: None,
            channels: vec![],
            allowed_users: vec![],
        });
        assert_eq!(ch.name(), "twitch");
    }

    #[test]
    fn style_prefix_content() {
        assert!(TWITCH_STYLE_PREFIX.contains("Twitch"));
        assert!(TWITCH_STYLE_PREFIX.contains("450"));
    }
}
