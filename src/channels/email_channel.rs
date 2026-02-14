#![allow(clippy::uninlined_format_args)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::redundant_closure_for_method_calls)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::trim_split_whitespace)]
#![allow(clippy::doc_link_with_quotes)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::unnecessary_map_or)]

use async_trait::async_trait;
use anyhow::{anyhow, Result};
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

fn default_imap_port() -> u16 { 993 }
fn default_smtp_port() -> u16 { 587 }
fn default_imap_folder() -> String { "INBOX".into() }
fn default_poll_interval() -> u64 { 60 }
fn default_true() -> bool { true }

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
        parsed.from()
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

    /// Fetch unseen emails via IMAP (blocking, run in spawn_blocking)
    fn fetch_unseen_imap(config: &EmailConfig) -> Result<Vec<(String, String, String, u64)>> {
        use rustls::ClientConfig as TlsConfig;
        use rustls_pki_types::ServerName;
        use std::sync::Arc;
        use tokio_rustls::rustls;

        // Connect TCP
        let tcp = TcpStream::connect((&*config.imap_host, config.imap_port))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30)))?;

        // TLS
        let mut root_store = rustls::RootCertStore::empty();
        root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        let tls_config = Arc::new(
            TlsConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth(),
        );
        let server_name: ServerName<'_> =
            ServerName::try_from(config.imap_host.clone())?;
        let conn =
            rustls::ClientConnection::new(tls_config, server_name)?;
        let mut tls = rustls::StreamOwned::new(conn, tcp);

        let read_line = |tls: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>| -> Result<String> {
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
        let _select = send_cmd(&mut tls, "A2", &format!("SELECT \"{}\"", config.imap_folder))?;

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
                            d.year as i32, u32::from(d.month), u32::from(d.day)
                        ).and_then(|date| date.and_hms_opt(u32::from(d.hour), u32::from(d.minute), u32::from(d.second)));
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
            let _ = send_cmd(&mut tls, &store_tag, &format!("STORE {uid} +FLAGS (\\Seen)"));
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
                            let mut seen = self.seen_messages.lock().unwrap();
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
