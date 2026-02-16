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
use async_trait::async_trait;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mail_parser::{MessageParser, MimeHeaders};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::Write as IoWrite;
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tracing::{error, info, warn};
use uuid::Uuid;

use super::traits::{Channel, ChannelMessage};

/// Email channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// SMTP server port (default: 587 for STARTTLS)
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
    /// Poll interval in seconds (default: 60)
    #[serde(default = "default_poll_interval")]
    pub poll_interval_secs: u64,
    /// Allowed sender addresses/domains (empty = deny all, ["*"] = allow all)
    #[serde(default)]
    pub allowed_senders: Vec<String>,
}

fn default_imap_port() -> u16 {
    993
}
fn default_smtp_port() -> u16 {
    587
}
fn default_imap_folder() -> String {
    "INBOX".into()
}
fn default_poll_interval() -> u64 {
    60
}
fn default_true() -> bool {
    true
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
            poll_interval_secs: default_poll_interval(),
            allowed_senders: Vec::new(),
        }
    }
}

/// Email channel — IMAP polling for inbound, SMTP for outbound
pub struct EmailChannel {
    pub config: EmailConfig,
    seen_messages: Mutex<HashSet<String>>,
}

impl EmailChannel {
    pub fn new(config: EmailConfig) -> Self {
        Self {
            config,
            seen_messages: Mutex::new(HashSet::new()),
        }
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
        result.split_whitespace().collect::<Vec<_>>().join(" ")
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

    fn build_imap_tls_config() -> Result<std::sync::Arc<tokio_rustls::rustls::ClientConfig>> {
        use rustls::ClientConfig as TlsConfig;
        use std::sync::Arc;
        use tokio_rustls::rustls;

        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

        let crypto_provider = rustls::crypto::CryptoProvider::get_default()
            .cloned()
            .unwrap_or_else(|| Arc::new(rustls::crypto::ring::default_provider()));

        let tls_config = TlsConfig::builder_with_provider(crypto_provider)
            .with_protocol_versions(rustls::DEFAULT_VERSIONS)?
            .with_root_certificates(root_store)
            .with_no_client_auth();

        Ok(Arc::new(tls_config))
    }

    /// Fetch unseen emails via IMAP (blocking, run in spawn_blocking)
    fn fetch_unseen_imap(config: &EmailConfig) -> Result<Vec<(String, String, String, u64)>> {
        use rustls_pki_types::ServerName;
        use tokio_rustls::rustls;

        // Connect TCP
        let tcp = TcpStream::connect((&*config.imap_host, config.imap_port))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;

        // TLS
        let tls_config = Self::build_imap_tls_config()?;
        let server_name: ServerName<'_> = ServerName::try_from(config.imap_host.clone())?;
        let conn = rustls::ClientConnection::new(tls_config, server_name)?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);

        let read_line =
            |tls: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>| -> Result<String> {
                let mut buf = Vec::new();
                loop {
                    let mut byte = [0u8; 1];
                    match std::io::Read::read(tls, &mut byte) {
                        Ok(0) => return Err(anyhow!("IMAP connection closed")),
                        Ok(_) => {
                            buf.push(byte[0]);
                            if buf.ends_with(b"\r\n") {
                                return Ok(String::from_utf8_lossy(&buf).to_string());
                            }
                        }
                        Err(e) => return Err(e.into()),
                    }
                }
            };

        let send_cmd = |tls: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
                        tag: &str,
                        cmd: &str|
         -> Result<Vec<String>> {
            let full = format!("{} {}\r\n", tag, cmd);
            IoWrite::write_all(tls, full.as_bytes())?;
            IoWrite::flush(tls)?;
            let mut lines = Vec::new();
            loop {
                let line = read_line(tls)?;
                let done = line.starts_with(tag);
                lines.push(line);
                if done {
                    break;
                }
            }
            Ok(lines)
        };

        // Read greeting
        let _greeting = read_line(&mut tls)?;

        // Login
        let login_resp = send_cmd(
            &mut tls,
            "A1",
            &format!("LOGIN \"{}\" \"{}\"", config.username, config.password),
        )?;
        if !login_resp.last().map_or(false, |l| l.contains("OK")) {
            return Err(anyhow!("IMAP login failed"));
        }

        // Select folder
        let _select = send_cmd(
            &mut tls,
            "A2",
            &format!("SELECT \"{}\"", config.imap_folder),
        )?;

        // Search unseen
        let search_resp = send_cmd(&mut tls, "A3", "SEARCH UNSEEN")?;
        let mut uids: Vec<&str> = Vec::new();
        for line in &search_resp {
            if line.starts_with("* SEARCH") {
                let parts: Vec<&str> = line.trim().split_whitespace().collect();
                if parts.len() > 2 {
                    uids.extend_from_slice(&parts[2..]);
                }
            }
        }

        let mut results = Vec::new();
        let mut tag_counter = 4_u32; // Start after A1, A2, A3

        for uid in &uids {
            // Fetch RFC822 with unique tag
            let fetch_tag = format!("A{}", tag_counter);
            tag_counter += 1;
            let fetch_resp = send_cmd(&mut tls, &fetch_tag, &format!("FETCH {} RFC822", uid))?;
            // Reconstruct the raw email from the response (skip first and last lines)
            let raw: String = fetch_resp
                .iter()
                .skip(1)
                .take(fetch_resp.len().saturating_sub(2))
                .cloned()
                .collect();

            if let Some(parsed) = MessageParser::default().parse(raw.as_bytes()) {
                let sender = Self::extract_sender(&parsed);
                let subject = parsed.subject().unwrap_or("(no subject)").to_string();
                let body = Self::extract_text(&parsed);
                let content = format!("Subject: {}\n\n{}", subject, body);
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

                results.push((msg_id, sender, content, ts));
            }

            // Mark as seen with unique tag
            let store_tag = format!("A{tag_counter}");
            tag_counter += 1;
            let _ = send_cmd(
                &mut tls,
                &store_tag,
                &format!("STORE {uid} +FLAGS (\\Seen)"),
            );
        }

        // Logout with unique tag
        let logout_tag = format!("A{tag_counter}");
        let _ = send_cmd(&mut tls, &logout_tag, "LOGOUT");

        Ok(results)
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

#[async_trait]
impl Channel for EmailChannel {
    fn name(&self) -> &str {
        "email"
    }

    async fn send(&self, message: &str, recipient: &str) -> Result<()> {
        let (subject, body) = if message.starts_with("Subject: ") {
            if let Some(pos) = message.find('\n') {
                (&message[9..pos], message[pos + 1..].trim())
            } else {
                ("ZeroClaw Message", message)
            }
        } else {
            ("ZeroClaw Message", message)
        };

        let email = Message::builder()
            .from(self.config.from_address.parse()?)
            .to(recipient.parse()?)
            .subject(subject)
            .body(body.to_string())?;

        let transport = self.create_smtp_transport()?;
        transport.send(&email)?;
        info!("Email sent to {}", recipient);
        Ok(())
    }

    async fn listen(&self, tx: mpsc::Sender<ChannelMessage>) -> Result<()> {
        info!(
            "Email polling every {}s on {}",
            self.config.poll_interval_secs, self.config.imap_folder
        );
        let mut tick = interval(Duration::from_secs(self.config.poll_interval_secs));
        let config = self.config.clone();

        loop {
            tick.tick().await;
            let cfg = config.clone();
            match tokio::task::spawn_blocking(move || Self::fetch_unseen_imap(&cfg)).await {
                Ok(Ok(messages)) => {
                    for (id, sender, content, ts) in messages {
                        {
                            let mut seen = self.seen_messages.lock().expect("seen_messages mutex should not be poisoned");
                            if seen.contains(&id) {
                                continue;
                            }
                            if !self.is_sender_allowed(&sender) {
                                warn!("Blocked email from {}", sender);
                                continue;
                            }
                            seen.insert(id.clone());
                        } // MutexGuard dropped before await
                        let msg = ChannelMessage {
                            id,
                            sender,
                            content,
                            channel: "email".to_string(),
                            timestamp: ts,
                        };
                        if tx.send(msg).await.is_err() {
                            return Ok(());
                        }
                    }
                }
                Ok(Err(e)) => {
                    error!("Email poll failed: {}", e);
                    sleep(Duration::from_secs(10)).await;
                }
                Err(e) => {
                    error!("Email poll task panicked: {}", e);
                    sleep(Duration::from_secs(10)).await;
                }
            }
        }
    }

    async fn health_check(&self) -> bool {
        let cfg = self.config.clone();
        tokio::task::spawn_blocking(move || {
            let tcp = TcpStream::connect((&*cfg.imap_host, cfg.imap_port));
            tcp.is_ok()
        })
        .await
        .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_imap_tls_config_succeeds() {
        let tls_config =
            EmailChannel::build_imap_tls_config().expect("TLS config construction should succeed");
        assert_eq!(std::sync::Arc::strong_count(&tls_config), 1);
    }

    #[test]
    fn bounded_seen_set_insert_and_contains() {
        let mut set = BoundedSeenSet::new(10);
        assert!(set.insert("a".into()));
        assert!(set.contains("a"));
        assert!(!set.contains("b"));
    }

    #[test]
    fn bounded_seen_set_rejects_duplicates() {
        let mut set = BoundedSeenSet::new(10);
        assert!(set.insert("a".into()));
        assert!(!set.insert("a".into()));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn bounded_seen_set_evicts_oldest_at_capacity() {
        let mut set = BoundedSeenSet::new(3);
        set.insert("a".into());
        set.insert("b".into());
        set.insert("c".into());
        assert_eq!(set.len(), 3);

        set.insert("d".into());
        assert_eq!(set.len(), 3);
        assert!(!set.contains("a"), "oldest entry should be evicted");
        assert!(set.contains("b"));
        assert!(set.contains("c"));
        assert!(set.contains("d"));
    }

    #[test]
    fn bounded_seen_set_evicts_in_fifo_order() {
        let mut set = BoundedSeenSet::new(2);
        set.insert("first".into());
        set.insert("second".into());
        set.insert("third".into());
        assert!(!set.contains("first"));
        assert!(set.contains("second"));
        assert!(set.contains("third"));

        set.insert("fourth".into());
        assert!(!set.contains("second"));
        assert!(set.contains("third"));
        assert!(set.contains("fourth"));
    }

    #[test]
    fn bounded_seen_set_capacity_one() {
        let mut set = BoundedSeenSet::new(1);
        set.insert("a".into());
        assert!(set.contains("a"));

        set.insert("b".into());
        assert!(!set.contains("a"));
        assert!(set.contains("b"));
        assert_eq!(set.len(), 1);
    }

    // EmailConfig tests

    #[test]
    fn email_config_default() {
        let config = EmailConfig::default();
        assert_eq!(config.imap_host, "");
        assert_eq!(config.imap_port, 993);
        assert_eq!(config.imap_folder, "INBOX");
        assert_eq!(config.smtp_host, "");
        assert_eq!(config.smtp_port, 587);
        assert!(config.smtp_tls);
        assert_eq!(config.username, "");
        assert_eq!(config.password, "");
        assert_eq!(config.from_address, "");
        assert_eq!(config.poll_interval_secs, 60);
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
            poll_interval_secs: 30,
            allowed_senders: vec!["allowed@example.com".to_string()],
        };
        assert_eq!(config.imap_host, "imap.example.com");
        assert_eq!(config.imap_folder, "Archive");
        assert_eq!(config.poll_interval_secs, 30);
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
            poll_interval_secs: 120,
            allowed_senders: vec!["*".to_string()],
        };
        let cloned = config.clone();
        assert_eq!(cloned.imap_host, config.imap_host);
        assert_eq!(cloned.smtp_port, config.smtp_port);
        assert_eq!(cloned.allowed_senders, config.allowed_senders);
    }

    // EmailChannel tests

    #[test]
    fn email_channel_new() {
        let config = EmailConfig::default();
        let channel = EmailChannel::new(config.clone());
        assert_eq!(channel.config.imap_host, config.imap_host);

        let seen_guard = channel
            .seen_messages
            .lock()
            .expect("seen_messages mutex should not be poisoned");
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
        assert_eq!(EmailChannel::strip_html("Text>with>brackets"), "Textwithbrackets");
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
    fn default_smtp_port_returns_587() {
        assert_eq!(default_smtp_port(), 587);
    }

    #[test]
    fn default_imap_folder_returns_inbox() {
        assert_eq!(default_imap_folder(), "INBOX");
    }

    #[test]
    fn default_poll_interval_returns_60() {
        assert_eq!(default_poll_interval(), 60);
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
            poll_interval_secs: 30,
            allowed_senders: vec!["allowed@example.com".to_string()],
        };

        let json = serde_json::to_string(&config).unwrap();
        let deserialized: EmailConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.imap_host, config.imap_host);
        assert_eq!(deserialized.smtp_port, config.smtp_port);
        assert_eq!(deserialized.allowed_senders, config.allowed_senders);
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
        assert_eq!(config.smtp_port, 587); // default
        assert!(config.smtp_tls); // default
        assert_eq!(config.poll_interval_secs, 60); // default
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
}
