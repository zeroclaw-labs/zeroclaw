use async_trait::async_trait;
use anyhow::{anyhow, Result};
use lettre::transport::smtp::authentication::Credentials;
use lettre::{Message, SmtpTransport, Transport};
use mail_parser::{Message as ParsedMessage, MimeHeaders};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::time::{interval, sleep};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// Email config — add to config.rs
use super::traits::{Channel, ChannelMessage};

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
        self.config.allowed_senders.iter().any(|allowed| {
            allowed.eq_ignore_ascii_case(email)
                || email.to_lowercase().ends_with(&format!("@{}", allowed.to_lowercase()))
                || (allowed.starts_with('@')
                    && email.to_lowercase().ends_with(&allowed.to_lowercase()))
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
        match parsed.from() {
            mail_parser::HeaderValue::Address(addr) => {
                addr.address.as_ref().map(|a| a.to_string()).unwrap_or_else(|| "unknown".into())
            }
            mail_parser::HeaderValue::AddressList(addrs) => {
                addrs.first()
                    .and_then(|a| a.address.as_ref())
                    .map(|a| a.to_string())
                    .unwrap_or_else(|| "unknown".into())
            }
            _ => "unknown".into(),
        }
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

        let mut read_line = |tls: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>| -> Result<String> {
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

        let mut send_cmd = |tls: &mut rustls::StreamOwned<rustls::ClientConnection, TcpStream>,
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

        for uid in &uids {
            // Fetch RFC822
            let fetch_resp = send_cmd(&mut tls, "A4", &format!("FETCH {} RFC822", uid))?;
            // Reconstruct the raw email from the response (skip first and last lines)
            let raw: String = fetch_resp
                .iter()
                .skip(1)
                .take(fetch_resp.len().saturating_sub(2))
                .cloned()
                .collect();

            if let Some(parsed) = ParsedMessage::parse(raw.as_bytes()) {
                let sender = Self::extract_sender(&parsed);
                let subject = parsed.subject().unwrap_or("(no subject)").to_string();
                let body = Self::extract_text(&parsed);
                let content = format!("Subject: {}\n\n{}", subject, body);
                let msg_id = parsed
                    .message_id()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| format!("gen-{}", Uuid::new_v4()));
                let ts = parsed
                    .date()
                    .map(|d| {
                        // DateTime year/month/day/hour/minute/second
                        let naive = chrono::NaiveDate::from_ymd_opt(
                            d.year as i32, d.month as u32, d.day as u32
                        ).and_then(|date| date.and_hms_opt(d.hour as u32, d.minute as u32, d.second as u32));
                        naive.map(|n| n.and_utc().timestamp() as u64).unwrap_or(0)
                    })
                    .unwrap_or_else(|| {
                        SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .unwrap()
                            .as_secs()
                    });

                results.push((msg_id, sender, content, ts));
            }

            // Mark as seen
            let _ = send_cmd(&mut tls, "A5", &format!("STORE {} +FLAGS (\\Seen)", uid));
        }

        // Logout
        let _ = send_cmd(&mut tls, "A6", "LOGOUT");

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
        match tokio::task::spawn_blocking(move || {
            let tcp = TcpStream::connect((&*cfg.imap_host, cfg.imap_port));
            tcp.is_ok()
        })
        .await
        {
            Ok(ok) => ok,
            Err(_) => false,
        }
    }
}
