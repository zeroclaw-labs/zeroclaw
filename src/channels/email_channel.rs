#![allow(clippy::uninlined_format_args)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::trim_split_whitespace)]
#![allow(clippy::doc_link_with_quotes)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unnecessary_map_or)]

use anyhow::{anyhow, Result};
use async_imap::extensions::idle::IdleResponse;
use async_imap::types::Fetch;
use async_imap::Session;
use async_trait::async_trait;
use futures_util::TryStreamExt;
use lettre::message::SinglePart;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mail_parser::{MessageParser, MimeHeaders};
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::DnsName;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio::time::{sleep, timeout};
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::traits::{Channel, ChannelMessage, SendMessage};

/// Email channel configuration
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EmailConfig {
    /// IMAP server hostname
    pub imap_host: String,
    /// IMAP server port (default: 993 for TLS)
    #[serde(default = "default_imap_port")]
    pub imap_port: u16,
    /// IMAP folder to poll (default: INBOX)
    #[serde(default = "default_imap_folder")]
    pub imap_folder: String,
    /// SMTP server hostname
    pub smtp_host: String,
    /// SMTP server port (default: 465 for TLS)
    #[serde(default = "default_smtp_port")]
    pub smtp_port: u16,
    /// Use TLS for SMTP (default: true)
    #[serde(default = "default_true")]
    pub smtp_tls: bool,
    /// Email username for authentication
    pub username: String,
    /// Email password for authentication
    pub password: String,
    /// From address for outgoing emails
    pub from_address: String,
    /// IDLE timeout in seconds before re-establishing connection (default: 1740 = 29 minutes)
    /// RFC 2177 recommends clients restart IDLE every 29 minutes
    #[serde(default = "default_idle_timeout", alias = "poll_interval_secs")]
    pub idle_timeout_secs: u64,
    /// Allowed sender addresses/domains (empty = deny all, ["*"] = allow all)
    #[serde(default)]
    pub allowed_senders: Vec<String>,
    /// Default subject line for outgoing emails (default: "ZeroClaw Message")
    #[serde(default = "default_subject")]
    pub default_subject: String,
}

impl crate::config::traits::ChannelConfig for EmailConfig {
    fn name() -> &'static str {
        "Email"
    }
    fn desc() -> &'static str {
        "Email over IMAP/SMTP"
    }
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    465
}
fn default_imap_folder() -> String {
    "INBOX".into()
}
fn default_idle_timeout() -> u64 {
    1740 // 29 minutes per RFC 2177
}
fn default_true() -> bool {
    true
}
fn default_subject() -> String {
    "ZeroClaw Message".into()
}

impl Default for EmailConfig {
    fn default() -> Self {
        Self {
            imap_host: String::new(),
            imap_port: default_imap_port(),
            imap_folder: default_imap_folder(),
            smtp_host: String::new(),
            smtp_port: default_smtp_port(),
            smtp_tls: true,
            username: String::new(),
            password: String::new(),
            from_address: String::new(),
            idle_timeout_secs: default_idle_timeout(),
            allowed_senders: Vec::new(),
            default_subject: default_subject(),
        }
    }
}

type ImapSession = Session<TlsStream<TcpStream>>;

/// Email channel — IMAP IDLE for instant push notifications, SMTP for outbound
pub struct EmailChannel {
    pub config: EmailConfig,
    seen_messages: Arc<Mutex<HashSet<String>>>,
    transcription_manager: Option<Arc<super::transcription::TranscriptionManager>>,
}

impl EmailChannel {
    pub fn new(config: EmailConfig) -> Self {
        Self {
            config,
            seen_messages: Arc::new(Mutex::new(HashSet::new())),
            transcription_manager: None,
        }
    }

    pub fn with_transcription(mut self, config: crate::config::TranscriptionConfig) -> Self {
        if !config.enabled {
            return self;
        }
        match super::transcription::TranscriptionManager::new(&config) {
            Ok(m) => {
                self.transcription_manager = Some(Arc::new(m));
            }
            Err(e) => {
                warn!("transcription manager init failed, voice transcription disabled: {e}");
            }
        }
        self
    }

    /// Check if a sender email is in the allowlist
    pub fn is_sender_allowed(&self, email: &str) -> bool {
        if self.config.allowed_senders.is_empty() {
            return false; // Empty = deny all
        }
        if self.config.allowed_senders.iter().any(|a| a == "*") {
            return true; // Wildcard = allow all
        }
        let email_lower = email.to_lowercase();
        self.config.allowed_senders.iter().any(|allowed| {
            if allowed.starts_with('@') {
                // Domain match with @ prefix: "@example.com"
                email_lower.ends_with(&allowed.to_lowercase())
            } else if allowed.contains('@') {
                // Full email address match
                allowed.eq_ignore_ascii_case(email)
            } else {
                // Domain match without @ prefix: "example.com"
                email_lower.ends_with(&format!("@{}", allowed.to_lowercase()))
            }
        })
    }

    /// Strip HTML tags from content (basic)
    pub fn strip_html(html: &str) -> String {
        let mut result = String::new();
        let mut in_tag = false;
        for ch in html.chars() {
            match ch {
                '<' => in_tag = true,
                '>' => in_tag = false,
                _ if !in_tag => result.push(ch),
                _ => {}
            }
        }
        let mut normalized = String::with_capacity(result.len());
        for word in result.split_whitespace() {
            if !normalized.is_empty() {
                normalized.push(' ');
            }
            normalized.push_str(word);
        }
        normalized
    }

    /// Extract the sender address from a parsed email
    fn extract_sender(parsed: &mail_parser::Message) -> String {
        parsed
            .from()
            .and_then(|addr| addr.first())
            .and_then(|a| a.address())
            .map(|s| s.to_string())
            .unwrap_or_else(|| "unknown".into())
    }

    /// Extract readable text from a parsed email
    fn extract_text(parsed: &mail_parser::Message) -> String {
        if let Some(text) = parsed.body_text(0) {
            return text.to_string();
        }
        if let Some(html) = parsed.body_html(0) {
            return Self::strip_html(html.as_ref());
        }
        for part in parsed.attachments() {
            let part: &mail_parser::MessagePart = part;
            if let Some(ct) = MimeHeaders::content_type(part) {
                if ct.ctype() == "text" {
                    if let Ok(text) = std::str::from_utf8(part.contents()) {
                        let name = MimeHeaders::attachment_name(part).unwrap_or("file");
                        return format!("[Attachment: {}]\n{}", name, text);
                    }
                }
            }
        }
        "(no readable content)".to_string()
    }

    /// Return the raw bytes and filename of the first audio/* MIME part, if any.
    fn extract_audio_attachment(parsed: &mail_parser::Message) -> Option<(Vec<u8>, String)> {
        let mut skipped: u32 = 0;
        let mut found: Option<(Vec<u8>, String)> = None;

        for part in parsed.attachments() {
            let part: &mail_parser::MessagePart = part;
            if let Some(ct) = MimeHeaders::content_type(part) {
                if ct.ctype().eq_ignore_ascii_case("audio") {
                    if found.is_none() {
                        let file_name = MimeHeaders::attachment_name(part)
                            .map(|n| n.to_string())
                            .filter(|n| !n.is_empty())
                            .unwrap_or_else(|| {
                                let ext = match ct
                                    .subtype()
                                    .unwrap_or("bin")
                                    .to_ascii_lowercase()
                                    .as_str()
                                {
                                    // Known audio subtypes → file extensions.
                                    // Unknown subtypes fall back to "bin", preserving
                                    // raw bytes as voice.bin for manual inspection.
                                    "mpeg" => "mp3",
                                    "ogg" => "ogg",
                                    "mp4" | "x-m4a" => "mp4",
                                    "aac" => "aac",
                                    "amr" => "amr",
                                    "wav" | "x-wav" => "wav",
                                    "webm" => "webm",
                                    "flac" => "flac",
                                    _ => "bin",
                                };
                                format!("voice.{ext}")
                            });
                        found = Some((part.contents().to_vec(), file_name));
                    } else {
                        skipped += 1;
                    }
                }
            }
        }

        if skipped > 0 {
            debug!("email: skipped {skipped} additional audio attachment(s)");
        }

        found
    }

    /// Connect to IMAP server with TLS and authenticate
    async fn connect_imap(&self) -> Result<ImapSession> {
        let addr = format!("{}:{}", self.config.imap_host, self.config.imap_port);
        debug!("Connecting to IMAP server at {}", addr);

        // Connect TCP
        let tcp = TcpStream::connect(&addr).await?;

        // Establish TLS using rustls
        let certs = RootCertStore {
            roots: webpki_roots::TLS_SERVER_ROOTS.into(),
        };
        let config = ClientConfig::builder()
            .with_root_certificates(certs)
            .with_no_client_auth();
        let tls_stream: TlsConnector = Arc::new(config).into();
        let sni: DnsName = self.config.imap_host.clone().try_into()?;
        let stream = tls_stream.connect(sni.into(), tcp).await?;

        // Create IMAP client
        let client = async_imap::Client::new(stream);

        // Login
        let session = client
            .login(&self.config.username, &self.config.password)
            .await
            .map_err(|(e, _)| anyhow!("IMAP login failed: {}", e))?;

        debug!("IMAP login successful");
        Ok(session)
    }

    /// Fetch and process unseen messages from the selected mailbox
    async fn fetch_unseen(&self, session: &mut ImapSession) -> Result<Vec<ParsedEmail>> {
        // Search for unseen messages
        let uids = session.uid_search("UNSEEN").await?;
        if uids.is_empty() {
            return Ok(Vec::new());
        }

        debug!("Found {} unseen messages", uids.len());

        let mut results = Vec::new();
        let uid_set: String = uids
            .iter()
            .map(|u| u.to_string())
            .collect::<Vec<_>>()
            .join(",");

        // Fetch message bodies
        let messages = session.uid_fetch(&uid_set, "RFC822").await?;
        let messages: Vec<Fetch> = messages.try_collect().await?;

        for msg in messages {
            let uid = msg.uid.unwrap_or(0);
            if let Some(body) = msg.body() {
                if let Some(parsed) = MessageParser::default().parse(body) {
                    let sender = Self::extract_sender(&parsed);
                    let subject = parsed.subject().unwrap_or("(no subject)").to_string();
                    let body_text = Self::extract_text(&parsed);
                    let content = format!("Subject: {}\n\n{}", subject, body_text);
                    let msg_id = parsed
                        .message_id()
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| format!("gen-{}", Uuid::new_v4()));

                    #[allow(clippy::cast_sign_loss)]
                    let ts = parsed
                        .date()
                        .map(|d| {
                            let naive = chrono::NaiveDate::from_ymd_opt(
                                d.year as i32,
                                u32::from(d.month),
                                u32::from(d.day),
                            )
                            .and_then(|date| {
                                date.and_hms_opt(
                                    u32::from(d.hour),
                                    u32::from(d.minute),
                                    u32::from(d.second),
                                )
                            });
                            naive.map_or(0, |n| n.and_utc().timestamp() as u64)
                        })
                        .unwrap_or_else(|| {
                            SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0)
                        });

                    let audio_attachment = Self::extract_audio_attachment(&parsed);

                    results.push(ParsedEmail {
                        _uid: uid,
                        msg_id,
                        sender,
                        content,
                        timestamp: ts,
                        audio_attachment,
                    });
                }
            }
        }

        // Mark fetched messages as seen
        if !results.is_empty() {
            let _ = session
                .uid_store(&uid_set, "+FLAGS (\\Seen)")
                .await?
                .try_collect::<Vec<_>>()
                .await;
        }

        Ok(results)
    }

    /// Run the IDLE loop, returning when a new message arrives or timeout
    /// Note: IDLE consumes the session and returns it via done()
    async fn wait_for_changes(
        &self,
        session: ImapSession,
    ) -> Result<(IdleWaitResult, ImapSession)> {
        let idle_timeout = Duration::from_secs(self.config.idle_timeout_secs);

        // Start IDLE mode - this consumes the session
        let mut idle = session.idle();
        idle.init().await?;

        debug!("Entering IMAP IDLE mode");

        // wait() returns (future, stop_source) - we only need the future
        let (wait_future, _stop_source) = idle.wait();

        // Wait for server notification or timeout
        let result = timeout(idle_timeout, wait_future).await;

        match result {
            Ok(Ok(response)) => {
                debug!("IDLE response: {:?}", response);
                // Done with IDLE, return session to normal mode
                let session = idle.done().await?;
                let wait_result = match response {
                    IdleResponse::NewData(_) => IdleWaitResult::NewMail,
                    IdleResponse::Timeout => IdleWaitResult::Timeout,
                    IdleResponse::ManualInterrupt => IdleWaitResult::Interrupted,
                };
                Ok((wait_result, session))
            }
            Ok(Err(e)) => {
                // Try to clean up IDLE state
                let _ = idle.done().await;
                Err(anyhow!("IDLE error: {}", e))
            }
            Err(_) => {
                // Timeout - RFC 2177 recommends restarting IDLE every 29 minutes
                debug!("IDLE timeout reached, will re-establish");
                let session = idle.done().await?;
                Ok((IdleWaitResult::Timeout, session))
            }
        }
    }

    /// Main IDLE-based listen loop with automatic reconnection
    async fn listen_with_idle(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        let mut backoff = Duration::from_secs(1);
        let max_backoff = Duration::from_secs(60);

        loop {
            match self.run_idle_session(&tx).await {
                Ok(()) => {
                    // Clean exit (channel closed)
                    return Ok(());
                }
                Err(e) => {
                    error!(
                        "IMAP session error: {}. Reconnecting in {:?}...",
                        e, backoff
                    );
                    sleep(backoff).await;
                    // Exponential backoff with cap
                    backoff = std::cmp::min(backoff * 2, max_backoff);
                }
            }
        }
    }

    /// Run a single IDLE session until error or clean shutdown
    async fn run_idle_session(&self, tx: &mpsc::Sender<ChannelMessage>) -> Result<()> {
        // Connect and authenticate
        let mut session = self.connect_imap().await?;

        // Select the mailbox
        session.select(&self.config.imap_folder).await?;
        info!(
            "Email IDLE listening on {} (instant push enabled)",
            self.config.imap_folder
        );

        // Check for existing unseen messages first
        self.process_unseen(&mut session, tx).await?;

        loop {
            // Enter IDLE and wait for changes (consumes session, returns it via result)
            match self.wait_for_changes(session).await {
                Ok((IdleWaitResult::NewMail, returned_session)) => {
                    debug!("New mail notification received");
                    session = returned_session;
                    self.process_unseen(&mut session, tx).await?;
                }
                Ok((IdleWaitResult::Timeout, returned_session)) => {
                    // Re-check for mail after IDLE timeout (defensive)
                    session = returned_session;
                    self.process_unseen(&mut session, tx).await?;
                }
                Ok((IdleWaitResult::Interrupted, _)) => {
                    info!("IDLE interrupted, exiting");
                    return Ok(());
                }
                Err(e) => {
                    // Connection likely broken, need to reconnect
                    return Err(e);
                }
            }
        }
    }

    /// Fetch unseen messages and send to channel
    async fn process_unseen(
        &self,
        session: &mut ImapSession,
        tx: &mpsc::Sender<ChannelMessage>,
    ) -> Result<()> {
        let messages = self.fetch_unseen(session).await?;

        for email in messages {
            // Check allowlist
            if !self.is_sender_allowed(&email.sender) {
                warn!("Blocked email from {}", email.sender);
                continue;
            }

            let is_new = {
                let mut seen = self.seen_messages.lock().await;
                seen.insert(email.msg_id.clone())
            };
            if !is_new {
                continue;
            }

            let mut content = email.content;

            if let (Some(manager), Some((audio_data, file_name))) = (
                self.transcription_manager.as_deref(),
                email.audio_attachment,
            ) {
                match manager.transcribe(&audio_data, &file_name).await {
                    Ok(transcript) => {
                        if content.ends_with("(no readable content)") {
                            // Audio-only email: replace placeholder with transcript
                            let prefix_end = content.len() - "(no readable content)".len();
                            content.truncate(prefix_end);
                            content.push_str("[Voice message transcript]\n");
                            content.push_str(&transcript);
                        } else {
                            content.push_str("\n\n[Voice message transcript]\n");
                            content.push_str(&transcript);
                        }
                    }
                    Err(e) => {
                        warn!("email audio transcription failed: {e}");
                    }
                }
            }

            let msg = ChannelMessage {
                id: email.msg_id,
                reply_target: email.sender.clone(),
                sender: email.sender,
                content,
                channel: "email".to_string(),
                timestamp: email.timestamp,
                thread_ts: None,
                interruption_scope_id: None,
            };

            if tx.send(msg).await.is_err() {
                // Channel closed, exit cleanly
                return Ok(());
            }
        }

        Ok(())
    }

    fn create_smtp_transport(&self) -> Result<SmtpTransport> {
        let creds = Credentials::new(self.config.username.clone(), self.config.password.clone());
        let transport = if self.config.smtp_tls {
            SmtpTransport::relay(&self.config.smtp_host)?
                .port(self.config.smtp_port)
                .credentials(creds)
                .build()
        } else {
            SmtpTransport::builder_dangerous(&self.config.smtp_host)
                .port(self.config.smtp_port)
                .credentials(creds)
                .build()
        };
        Ok(transport)
    }
}

/// Internal struct for parsed email data
struct ParsedEmail {
    _uid: u32,
    msg_id: String,
    sender: String,
    content: String,
    timestamp: u64,
    audio_attachment: Option<(Vec<u8>, String)>,
}

/// Result from waiting on IDLE
enum IdleWaitResult {
    NewMail,
    Timeout,
    Interrupted,
}

#[async_trait]
impl Channel for EmailChannel {
    fn name(&self) -> &str {
        "email"
    }

    async fn send(&self, message: &SendMessage) -> Result<()> {
        // Use explicit subject if provided, otherwise fall back to legacy parsing or default
        let default_subject = self.config.default_subject.as_str();
        let (subject, body) = if let Some(ref subj) = message.subject {
            (subj.as_str(), message.content.as_str())
        } else if message.content.starts_with("Subject: ") {
            if let Some(pos) = message.content.find('\n') {
                (&message.content[9..pos], message.content[pos + 1..].trim())
            } else {
                (default_subject, message.content.as_str())
            }
        } else {
            (default_subject, message.content.as_str())
        };

        let email = Message::builder()
            .from(self.config.from_address.parse()?)
            .to(message.recipient.parse()?)
            .subject(subject)
            .singlepart(SinglePart::plain(body.to_string()))?;

        let transport = self.create_smtp_transport()?;
        transport.send(&email)?;
        info!("Email sent to {}", message.recipient);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        info!(
            "Starting email channel with IDLE support on {}",
            self.config.imap_folder
        );
        self.listen_with_idle(tx).await
    }

    async fn health_check(&self) -> bool {
        // Fully async health check - attempt IMAP connection
        match timeout(Duration::from_secs(10), self.connect_imap()).await {
            Ok(Ok(mut session)) => {
                // Try to logout cleanly
                let _ = session.logout().await;
                true
            }
            Ok(Err(e)) => {
                debug!("Health check failed: {}", e);
                false
            }
            Err(_) => {
                debug!("Health check timed out");
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_smtp_port_uses_tls_port() {
        assert_eq!(default_smtp_port(), 465);
    }

    #[test]
    fn email_config_default_uses_tls_smtp_defaults() {
        let config = EmailConfig::default();
        assert_eq!(config.smtp_port, 465);
        assert!(config.smtp_tls);
    }

    #[test]
    fn default_idle_timeout_is_29_minutes() {
        assert_eq!(default_idle_timeout(), 1740);
    }

    #[tokio::test]
    async fn seen_messages_starts_empty() {
        let channel = EmailChannel::new(EmailConfig::default());
        let seen = channel.seen_messages.lock().await;
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn seen_messages_tracks_unique_ids() {
        let channel = EmailChannel::new(EmailConfig::default());
        let mut seen = channel.seen_messages.lock().await;

        assert!(seen.insert("first-id".to_string()));
        assert!(!seen.insert("first-id".to_string()));
        assert!(seen.insert("second-id".to_string()));
        assert_eq!(seen.len(), 2);
    }

    // EmailConfig tests

    #[test]
    fn email_config_default() {
        let config = EmailConfig::default();
        assert_eq!(config.imap_host, "");
        assert_eq!(config.imap_port, 993);
        assert_eq!(config.imap_folder, "INBOX");
        assert_eq!(config.smtp_host, "");
        assert_eq!(config.smtp_port, 465);
        assert!(config.smtp_tls);
        assert_eq!(config.username, "");
        assert_eq!(config.password, "");
        assert_eq!(config.from_address, "");
        assert_eq!(config.idle_timeout_secs, 1740);
        assert!(config.allowed_senders.is_empty());
    }

    #[test]
    fn email_config_custom() {
        let config = EmailConfig {
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            imap_folder: "Archive".to_string(),
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 465,
            smtp_tls: true,
            username: "user@example.com".to_string(),
            password: "pass123".to_string(),
            from_address: "bot@example.com".to_string(),
            idle_timeout_secs: 1200,
            allowed_senders: vec!["allowed@example.com".to_string()],
            default_subject: "Custom Subject".to_string(),
        };
        assert_eq!(config.imap_host, "imap.example.com");
        assert_eq!(config.imap_folder, "Archive");
        assert_eq!(config.idle_timeout_secs, 1200);
        assert_eq!(config.default_subject, "Custom Subject");
    }

    #[test]
    fn email_config_clone() {
        let config = EmailConfig {
            imap_host: "imap.test.com".to_string(),
            imap_port: 993,
            imap_folder: "INBOX".to_string(),
            smtp_host: "smtp.test.com".to_string(),
            smtp_port: 587,
            smtp_tls: true,
            username: "user@test.com".to_string(),
            password: "secret".to_string(),
            from_address: "bot@test.com".to_string(),
            idle_timeout_secs: 1740,
            allowed_senders: vec!["*".to_string()],
            default_subject: "Test Subject".to_string(),
        };
        let cloned = config.clone();
        assert_eq!(cloned.imap_host, config.imap_host);
        assert_eq!(cloned.smtp_port, config.smtp_port);
        assert_eq!(cloned.allowed_senders, config.allowed_senders);
        assert_eq!(cloned.default_subject, config.default_subject);
    }

    // EmailChannel tests

    #[tokio::test]
    async fn email_channel_new() {
        let config = EmailConfig::default();
        let channel = EmailChannel::new(config.clone());
        assert_eq!(channel.config.imap_host, config.imap_host);

        let seen_guard = channel.seen_messages.lock().await;
        assert_eq!(seen_guard.len(), 0);
    }

    #[test]
    fn email_channel_name() {
        let channel = EmailChannel::new(EmailConfig::default());
        assert_eq!(channel.name(), "email");
    }

    // is_sender_allowed tests

    #[test]
    fn is_sender_allowed_empty_list_denies_all() {
        let config = EmailConfig {
            allowed_senders: vec![],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(!channel.is_sender_allowed("anyone@example.com"));
        assert!(!channel.is_sender_allowed("user@test.com"));
    }

    #[test]
    fn is_sender_allowed_wildcard_allows_all() {
        let config = EmailConfig {
            allowed_senders: vec!["*".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("anyone@example.com"));
        assert!(channel.is_sender_allowed("user@test.com"));
        assert!(channel.is_sender_allowed("random@domain.org"));
    }

    #[test]
    fn is_sender_allowed_specific_email() {
        let config = EmailConfig {
            allowed_senders: vec!["allowed@example.com".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("allowed@example.com"));
        assert!(!channel.is_sender_allowed("other@example.com"));
        assert!(!channel.is_sender_allowed("allowed@other.com"));
    }

    #[test]
    fn is_sender_allowed_domain_with_at_prefix() {
        let config = EmailConfig {
            allowed_senders: vec!["@example.com".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("user@example.com"));
        assert!(channel.is_sender_allowed("admin@example.com"));
        assert!(!channel.is_sender_allowed("user@other.com"));
    }

    #[test]
    fn is_sender_allowed_domain_without_at_prefix() {
        let config = EmailConfig {
            allowed_senders: vec!["example.com".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("user@example.com"));
        assert!(channel.is_sender_allowed("admin@example.com"));
        assert!(!channel.is_sender_allowed("user@other.com"));
    }

    #[test]
    fn is_sender_allowed_case_insensitive() {
        let config = EmailConfig {
            allowed_senders: vec!["Allowed@Example.COM".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("allowed@example.com"));
        assert!(channel.is_sender_allowed("ALLOWED@EXAMPLE.COM"));
        assert!(channel.is_sender_allowed("AlLoWeD@eXaMpLe.cOm"));
    }

    #[test]
    fn is_sender_allowed_multiple_senders() {
        let config = EmailConfig {
            allowed_senders: vec![
                "user1@example.com".to_string(),
                "user2@test.com".to_string(),
                "@allowed.com".to_string(),
            ],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("user1@example.com"));
        assert!(channel.is_sender_allowed("user2@test.com"));
        assert!(channel.is_sender_allowed("anyone@allowed.com"));
        assert!(!channel.is_sender_allowed("user3@example.com"));
    }

    #[test]
    fn is_sender_allowed_wildcard_with_specific() {
        let config = EmailConfig {
            allowed_senders: vec!["*".to_string(), "specific@example.com".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(channel.is_sender_allowed("anyone@example.com"));
        assert!(channel.is_sender_allowed("specific@example.com"));
    }

    #[test]
    fn is_sender_allowed_empty_sender() {
        let config = EmailConfig {
            allowed_senders: vec!["@example.com".to_string()],
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert!(!channel.is_sender_allowed(""));
        // "@example.com" ends with "@example.com" so it's allowed
        assert!(channel.is_sender_allowed("@example.com"));
    }

    // strip_html tests

    #[test]
    fn strip_html_basic() {
        assert_eq!(EmailChannel::strip_html("<p>Hello</p>"), "Hello");
        assert_eq!(EmailChannel::strip_html("<div>World</div>"), "World");
    }

    #[test]
    fn strip_html_nested_tags() {
        assert_eq!(
            EmailChannel::strip_html("<div><p>Hello <strong>World</strong></p></div>"),
            "Hello World"
        );
    }

    #[test]
    fn strip_html_multiple_lines() {
        let html = "<div>\n  <p>Line 1</p>\n  <p>Line 2</p>\n</div>";
        assert_eq!(EmailChannel::strip_html(html), "Line 1 Line 2");
    }

    #[test]
    fn strip_html_preserves_text() {
        assert_eq!(EmailChannel::strip_html("No tags here"), "No tags here");
        assert_eq!(EmailChannel::strip_html(""), "");
    }

    #[test]
    fn strip_html_handles_malformed() {
        assert_eq!(EmailChannel::strip_html("<p>Unclosed"), "Unclosed");
        // The function removes everything between < and >, so "Text>with>brackets" becomes "Textwithbrackets"
        assert_eq!(
            EmailChannel::strip_html("Text>with>brackets"),
            "Textwithbrackets"
        );
    }

    #[test]
    fn strip_html_self_closing_tags() {
        // Self-closing tags are removed but don't add spaces
        assert_eq!(EmailChannel::strip_html("Hello<br/>World"), "HelloWorld");
        assert_eq!(EmailChannel::strip_html("Text<hr/>More"), "TextMore");
    }

    #[test]
    fn strip_html_attributes_preserved() {
        assert_eq!(
            EmailChannel::strip_html("<a href=\"http://example.com\">Link</a>"),
            "Link"
        );
    }

    #[test]
    fn strip_html_multiple_spaces_collapsed() {
        assert_eq!(
            EmailChannel::strip_html("<p>Word</p>  <p>Word</p>"),
            "Word Word"
        );
    }

    #[test]
    fn strip_html_special_characters() {
        assert_eq!(
            EmailChannel::strip_html("<span>&lt;tag&gt;</span>"),
            "&lt;tag&gt;"
        );
    }

    // Default function tests

    #[test]
    fn default_imap_port_returns_993() {
        assert_eq!(default_imap_port(), 993);
    }

    #[test]
    fn default_smtp_port_returns_465() {
        assert_eq!(default_smtp_port(), 465);
    }

    #[test]
    fn default_imap_folder_returns_inbox() {
        assert_eq!(default_imap_folder(), "INBOX");
    }

    #[test]
    fn default_true_returns_true() {
        assert!(default_true());
    }

    // EmailConfig serialization tests

    #[test]
    fn email_config_serialize_deserialize() {
        let config = EmailConfig {
            imap_host: "imap.example.com".to_string(),
            imap_port: 993,
            imap_folder: "INBOX".to_string(),
            smtp_host: "smtp.example.com".to_string(),
            smtp_port: 587,
            smtp_tls: true,
            username: "user@example.com".to_string(),
            password: "password123".to_string(),
            from_address: "bot@example.com".to_string(),
            idle_timeout_secs: 1740,
            allowed_senders: vec!["allowed@example.com".to_string()],
            default_subject: "Serialization Test".to_string(),
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: EmailConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.imap_host, config.imap_host);
        assert_eq!(deserialized.smtp_port, config.smtp_port);
        assert_eq!(deserialized.allowed_senders, config.allowed_senders);
        assert_eq!(deserialized.default_subject, config.default_subject);
    }

    #[test]
    fn email_config_deserialize_with_defaults() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com"
        }"#;

        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.imap_port, 993); // default
        assert_eq!(config.smtp_port, 465); // default
        assert!(config.smtp_tls); // default
        assert_eq!(config.idle_timeout_secs, 1740); // default
        assert_eq!(config.default_subject, "ZeroClaw Message"); // default
    }

    #[test]
    fn idle_timeout_deserializes_explicit_value() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com",
            "idle_timeout_secs": 900
        }"#;
        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.idle_timeout_secs, 900);
    }

    #[test]
    fn idle_timeout_deserializes_legacy_poll_interval_alias() {
        let json = r#"{
            "imap_host": "imap.test.com",
            "smtp_host": "smtp.test.com",
            "username": "user",
            "password": "pass",
            "from_address": "bot@test.com",
            "poll_interval_secs": 120
        }"#;
        let config: EmailConfig = serde_json::from_str(json).unwrap();
        assert_eq!(config.idle_timeout_secs, 120);
    }

    #[test]
    fn idle_timeout_propagates_to_channel() {
        let config = EmailConfig {
            idle_timeout_secs: 600,
            ..Default::default()
        };
        let channel = EmailChannel::new(config);
        assert_eq!(channel.config.idle_timeout_secs, 600);
    }

    #[test]
    fn email_config_debug_output() {
        let config = EmailConfig {
            imap_host: "imap.debug.com".to_string(),
            ..Default::default()
        };
        let debug_str = format!("{:?}", config);
        assert!(debug_str.contains("imap.debug.com"));
    }

    // ── Transcription tests ─────────────────────────────────────────

    #[test]
    fn email_manager_none_when_transcription_not_configured() {
        let channel = EmailChannel::new(EmailConfig::default());
        assert!(channel.transcription_manager.is_none());
    }

    #[test]
    fn email_manager_some_when_valid_config() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".into(),
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: "http://localhost:8001/v1/transcribe".into(),
                bearer_token: "test-token".into(),
                max_audio_bytes: 25 * 1024 * 1024,
                timeout_secs: 120,
            }),
            ..Default::default()
        };
        let channel = EmailChannel::new(EmailConfig::default()).with_transcription(config);
        assert!(channel.transcription_manager.is_some());
    }

    #[test]
    fn email_manager_none_and_warn_on_init_failure() {
        let config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "groq".to_string(),
            api_key: Some(String::new()),
            ..Default::default()
        };
        let channel = EmailChannel::new(EmailConfig::default()).with_transcription(config);
        assert!(channel.transcription_manager.is_none());
    }

    #[test]
    fn extract_audio_attachment_returns_none_for_text_only() {
        let raw = b"From: test@example.com\r\nSubject: Test\r\n\r\nPlain text email";
        let parsed = MessageParser::default().parse(raw).unwrap();
        assert_eq!(EmailChannel::extract_audio_attachment(&parsed), None);
    }

    #[test]
    fn extract_audio_attachment_finds_ogg_part() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: text/plain\r\n\
\r\n\
Message text\r\n\
--boundary123\r\n\
Content-Type: audio/ogg\r\n\
\r\n\
fake-audio-bytes\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (bytes, filename) = result.unwrap();
        assert_eq!(bytes, b"fake-audio-bytes");
        assert_eq!(filename, "voice.ogg");
    }

    #[test]
    fn extract_audio_attachment_uses_filename_header() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: text/plain\r\n\
\r\n\
Message text\r\n\
--boundary123\r\n\
Content-Type: audio/mp4\r\n\
Content-Disposition: attachment; filename=\"memo.m4a\"\r\n\
\r\n\
fake-audio\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (bytes, filename) = result.unwrap();
        assert_eq!(bytes, b"fake-audio");
        assert_eq!(filename, "memo.m4a");
    }

    #[test]
    fn extract_audio_attachment_returns_first_of_multiple() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/ogg\r\n\
\r\n\
first-audio\r\n\
--boundary123\r\n\
Content-Type: audio/wav\r\n\
\r\n\
second-audio\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (bytes, filename) = result.unwrap();
        assert_eq!(bytes, b"first-audio");
        assert_eq!(filename, "voice.ogg");
    }

    #[test]
    fn extract_audio_attachment_fallback_ext_mpeg() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/mpeg\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.mp3");
    }

    #[test]
    fn extract_audio_attachment_fallback_ext_webm() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/webm\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.webm");
    }

    #[test]
    fn extract_audio_attachment_fallback_ext_aac() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/aac\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.aac");
    }

    #[test]
    fn extract_audio_attachment_fallback_ext_amr() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/amr\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.amr");
    }

    #[test]
    fn extract_audio_attachment_fallback_ext_x_m4a() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/x-m4a\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.mp4");
    }

    #[test]
    fn extract_audio_attachment_unknown_subtype_falls_back_to_bin() {
        let raw = b"From: test@example.com\r\n\
Subject: Test\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: audio/x-custom\r\n\
\r\n\
audio-data\r\n\
--boundary123--\r\n";
        let parsed = MessageParser::default().parse(raw).unwrap();
        let result = EmailChannel::extract_audio_attachment(&parsed);
        assert!(result.is_some());
        let (_bytes, filename) = result.unwrap();
        assert_eq!(filename, "voice.bin");
    }

    #[tokio::test]
    async fn process_unseen_skips_transcription_when_manager_none() {
        let channel = EmailChannel::new(EmailConfig {
            allowed_senders: vec!["*".to_string()],
            ..Default::default()
        });
        assert!(channel.transcription_manager.is_none());

        // Simulate a ParsedEmail with audio attachment
        let parsed_email = ParsedEmail {
            _uid: 1,
            msg_id: "test-msg-1".to_string(),
            sender: "test@example.com".to_string(),
            content: "Subject: Test\n\nText body".to_string(),
            timestamp: 0,
            audio_attachment: Some((b"fake-audio".to_vec(), "voice.ogg".to_string())),
        };

        {
            let mut seen = channel.seen_messages.lock().await;
            seen.insert(parsed_email.msg_id.clone());
        }

        // Manually exercise the transcription logic from process_unseen
        let mut content = parsed_email.content.clone();
        if let (Some(manager), Some((audio_data, file_name))) = (
            channel.transcription_manager.as_deref(),
            parsed_email.audio_attachment,
        ) {
            match manager.transcribe(&audio_data, &file_name).await {
                Ok(transcript) => {
                    content.push_str("\n\n[Voice message transcript]\n");
                    content.push_str(&transcript);
                }
                Err(e) => {
                    warn!("email audio transcription failed: {e}");
                }
            }
        }

        // Content should remain unchanged (no transcript appended)
        assert_eq!(content, "Subject: Test\n\nText body");
        assert!(!content.contains("[Voice message transcript]"));
    }

    #[tokio::test]
    async fn process_unseen_keeps_text_when_transcription_fails() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        // Configure mock to return 500 error
        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .respond_with(ResponseTemplate::new(500).set_body_string("Internal Server Error"))
            .mount(&mock_server)
            .await;

        let transcription_config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".to_string(),
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", mock_server.uri()),
                bearer_token: "test-token".to_string(),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
            ..Default::default()
        };

        let channel =
            EmailChannel::new(EmailConfig::default()).with_transcription(transcription_config);

        assert!(channel.transcription_manager.is_some());

        let manager = channel.transcription_manager.as_ref().unwrap();
        let audio_data = b"fake-audio";
        let file_name = "voice.ogg";

        // Transcription should fail
        let result = manager.transcribe(audio_data, file_name).await;
        assert!(result.is_err());

        // Simulate the process_unseen logic
        let content = "Subject: Test\n\nText body".to_string();
        if let Err(e) = result {
            warn!("email audio transcription failed: {e}");
            // Content remains unchanged
        }

        // Content should be preserved without transcript
        assert_eq!(content, "Subject: Test\n\nText body");
        assert!(!content.contains("[Voice message transcript]"));
    }

    #[tokio::test]
    async fn email_audio_routes_through_local_whisper() {
        use wiremock::matchers::{header_exists, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let mock_server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/v1/transcribe"))
            .and(header_exists("authorization"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"text": "hello world"})),
            )
            .mount(&mock_server)
            .await;

        let transcription_config = crate::config::TranscriptionConfig {
            enabled: true,
            default_provider: "local_whisper".to_string(),
            local_whisper: Some(crate::config::LocalWhisperConfig {
                url: format!("{}/v1/transcribe", mock_server.uri()),
                bearer_token: "test-token".to_string(),
                max_audio_bytes: 10 * 1024 * 1024,
                timeout_secs: 30,
            }),
            ..Default::default()
        };

        let channel =
            EmailChannel::new(EmailConfig::default()).with_transcription(transcription_config);

        // Build a synthetic multipart/mixed email with audio/ogg attachment
        let raw_email = b"From: test@example.com\r\n\
Subject: Voice Message\r\n\
MIME-Version: 1.0\r\n\
Content-Type: multipart/mixed; boundary=\"boundary123\"\r\n\
\r\n\
--boundary123\r\n\
Content-Type: text/plain\r\n\
\r\n\
Text content\r\n\
--boundary123\r\n\
Content-Type: audio/ogg\r\n\
\r\n\
fake-ogg-bytes\r\n\
--boundary123--\r\n";

        let parsed = MessageParser::default().parse(raw_email).unwrap();
        let audio_attachment = EmailChannel::extract_audio_attachment(&parsed);
        assert!(audio_attachment.is_some());

        let (audio_data, file_name) = audio_attachment.unwrap();
        let manager = channel.transcription_manager.as_ref().unwrap();
        let transcript = manager.transcribe(&audio_data, &file_name).await.unwrap();
        assert_eq!(transcript, "hello world");
    }
}
